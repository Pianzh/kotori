# 架构总览

**这篇讲什么**:kotori 是什么、两端的能力边界在哪、两个进程各自拥有什么、一局游戏从点击「启动」到退出后上传要走哪些步骤、源码怎么分模块。

**什么时候读它**:第一次打开这个仓库;或者要确认「某件事该归谁管」。

其余四篇按主题拆开,索引在 [README.md](README.md)。

核对来源:`src/main.rs`、`src/cli/`、`src/daemon/`、`src/config/`、`src/scale/`、`src/sync/`、`src/ui/`。

---

## 1. 定位与能力边界

kotori 是一个 Galgame 管理器,主业两件:多设备之间同步存档,以及在 Linux 上给 wine 下的老游戏加缩放。界面形态与使用场景参考 [Magpie](https://github.com/Blinue/Magpie);分类、封面、元数据、VNDB 这些 gal 周边功能刻意不做。

两端能力**不对等**,缩放是唯一的不对称项:

| 平台 | 启动游戏 | 进程追踪与退出后上传 | 运行时缩放 | 云存档同步 |
|------|---------|--------------------|-----------|-----------|
| Linux x86_64 | ✅ wine + gamescope,或直接启动 exe | ✅ | ✅ | ✅ |
| Linux aarch64 | ✅ | ✅ | ⚠️ 未验证 | ✅ |
| Windows x64 | ✅ 直接启动 exe | ✅ | ❌ 归 Magpie | ✅ |

Windows 上的缩放不归 kotori 管。Magpie 留给第三方的接口只有观察能力,没有控制接口,所以 kotori 在那边的缩放后端是一个**故意留空**的实现(`src/scale/unsupported.rs`):它管的会话是真的(直接启动、进程追踪、退出后上传全都照常),只有缩放动作回「做不到」。这一节最容易写错的地方就在这儿 —— 那边不是「还没写完」,是「不归我们管」。

### 几条硬规矩

- **一切启动都必须经 IPC 交给 daemon**。GUI 与 CLI 都先 `daemon::ensure_running` 再发 `game.launch`,没有任何一条路径在 UI/CLI 进程里直接 spawn gamescope。
- **`config.toml` 是用户本地数据**。游戏清单、挂载点、存档路径只出现在用户机器上;文档与测试里只用占位路径。
- **凭据绝不进 `config.toml`**,只在 [cloud-sync.md](cloud-sync.md) §10 那几个存储之一。
- **两端都能编译**。传输层是平台抽象:Unix 上是 Unix socket,Windows 上是命名管道;纯 Unix 的实现(gamescope、X11、进程组收尾)整个在 `cfg(unix)` 里,Windows 侧对应一个空后端。

技术栈:`Rust 2024` + `tokio` + `Slint 1.17`(GUI)+ 外部工具 `gamescope` / `wine` / `kopia`(默认同步引擎,release 锁 0.23.1)与备选的 `rclone`。外部程序都包在可注入的查找函数后面,见 [process-and-ipc.md](process-and-ipc.md) §6。

---

## 2. 两个进程与所有权

```
┌───────────────────────────┐        Unix socket / 命名管道,一行一条 JSON-RPC 2.0
│ kotori ui   (Slint 窗口)  │  ────────────────────────────────────────┐
│ kotori <cli 子命令>       │  ◄────────────────────────────────────────┤
└───────────────────────────┘                                          │
        │ 只有 daemon 不在时才 spawn 它(ensure_running)                 ▼
        │                                              ┌──────────────────────────────┐
        └─────────────────────────────────────────────►│ kotori daemon                │
                                                       │  config(RwLock) 唯一写者     │
                                                       │  sessions(HashMap) 唯一真相  │
                                                       │  凭据句柄唯一持有者            │
                                                       └──────────┬───────────────────┘
                                                                  │ spawn + 进程组
                                                                  ▼
                                                    gamescope → wine → 游戏
```

| 东西 | 谁拥有 | 代码位置 |
|------|--------|----------|
| 配置文件(读 + 写) | **daemon 独占** | `daemon::Daemon::mutate_config`(先改副本、落盘、再提交内存) |
| 运行中的会话 | **daemon 独占,只有一份** | `scale::ScaleSession` 表,由具体后端持有 |
| 凭据(密钥环/凭据文件句柄) | **daemon 独占** | `daemon::sync_rpc::SyncState` |
| 游戏进程 | daemon spawn,daemon 收尾 | `scale/gamescope.rs`,收尾见 `scale/teardown.rs` |
| 窗口/页签状态、正在编辑的草稿 | UI 进程 | `src/ui/app/`、`src/ui/slint/` |
| 日志 | 各自写各自的文件 | UI → `<data_dir>/logs/ui.log`;daemon → `<data_dir>/logs/daemon.log` |

两条容易被写错的边界:

1. **`daemon.shutdown` 不碰正在玩的游戏**。它只让 accept 循环退出;**只有信号**(SIGTERM/SIGINT)才把游戏一并收尾。
2. **UI 不许「自愈」掉用户按下的「停止服务」**。`App::daemon_paused` 一旦立起,周期刷新与失败退避重试都走 `load_without_booting`,`ensure_running` 不再被调用。

---

## 3. 一局游戏的完整生命周期

以 GUI 点「启动」为例(CLI `kotori launch <id>` 多一步:额外等 `game.wait` 阻塞到会话消失)。

```
①点启动 ②game.launch ③启动前取回 ④spawn gamescope ⑤Started
   │         │            │              │             │
   │         │            │              │             └─► ⑥UI 每 3s 轮询 daemon.status
   │         │            │              └─► 会话入 sessions 表,CWD=游戏根目录,WINEPREFIX 显式注入
   │         │            └─► 拉最新一版包,逐文件比较只取新的(上限 30s,失败只报告不拦)
   │         └─► JSON-RPC over 本机连接(§2)
   └─► Message::Launch → Task::perform(rpc) → 回包 Message::LaunchDone

⑦游戏结束(三条路,见 scaling.md §5) ⑧收尾 ⑨Ended 事件 ⑩等 3s ⑪把整份存档打成一包上传
```

| # | 步骤 | 关键点 |
|---|------|--------|
| 1 | UI 发 `Message::Launch(id)`,`launching` 置位 | 效果由 `ui/driver.rs::spawn` 跑在 tokio 上 |
| 2 | `game.launch` → daemon | daemon 先解析 prefix,再开会话 |
| 3 | **启动前自动取回**(同步开启且有存档位置时) | `sync_pull_before_launch`:取云端最新一版包,按清单只铺比本机新的文件,预算 `PULL_TIMEOUT` 30s,超时也照常启动 |
| 4 | 组 gamescope 命令行 → spawn | `wine::resolve_prefix` → `scale::build_gamescope_args` → `process_group(0)` |
| 5 | 会话登记 + 广播 `SessionKind::Started` | sessions 表是「什么在跑」的唯一真相 |
| 6 | UI 每 `STATUS_POLL`(3s)拉 `daemon.status` | 列表因此显示「运行中」;观测会话也在里面 |
| 7 | **游戏结束** | 三条路径(叉号 / 游戏内部退出 / 信号),详见 [scaling.md](scaling.md) §5 |
| 8 | 收尾:进程组 + 子进程树,再 `wineserver -k` | `scale::teardown::terminate_session` + `wine::close_prefix` |
| 9 | watcher 移除会话并广播 `SessionKind::Ended` | `daemon::spawn_sync_events` 只认 `Ended` |
| 10 | 等 `SETTLE_DELAY`(3s) | 让 wineserver 把刚写的存档刷到盘上 |
| 11 | **退出后自动上传** | `sync_after_game_exit` → `Runner::upload`:收集存档位置 → 打成一版一个的完整包 → 交给引擎传上云 |

**失败面**:`game.launch` 在 gamescope 300ms 内立刻退出时返回错误,UI 落在顶部错误条;取回失败只写进回包的 `sync_pull` 字段与日志,**不拦启动**。同步的任何一步都不该把用户挡在游戏外面。

---

## 4. 模块地图

| 模块 | 负责什么 | 主要文件 |
|------|----------|----------|
| 入口 / CLI | 子命令分发、日志初始化、`scale`/`sync` 的共用汇报 | `src/main.rs`、`src/cli/{mod,add_cli,scale_cli,sync_cli}.rs` |
| IPC 客户端 | 一行 JSON-RPC 发一条收一条 | `src/rpc.rs` |
| daemon | 分发请求、**唯一配置写者**、信号处理、会话事件 → 同步钩子 | `src/daemon/{mod,dispatch,ipc,watch,index_refresh}.rs` |
| daemon 处理器 | 按域切开:`game_rpc` / `scale_rpc` / `status_rpc` / `sync_rpc/`,wire 结构在 `protocol.rs` | `src/daemon/{game_rpc,game_write,scale_rpc,status_rpc}.rs`、`src/daemon/sync_rpc/` |
| 配置 | 类型、读写、路径解析(全部可注入)、挂载点解析、跨进程文件锁、缩放档案、同步设置 | `src/config/{mod,locations,lock,sync}.rs`、`src/config/{paths.rs,paths/,profile/}` |
| 缩放引擎 | gamescope 命令行、会话、运行时属性、退出看门狗;Windows 上是空后端 | `src/scale/{action,args,gamescope,teardown,x11,direct}.rs`、`src/scale/{mod,unsupported}.rs` |
| 桌面集成 | 只做一件事:KDE 的 KWin 脚本桥(窗口尺寸/全屏) | `src/desktop/{mod,kde}.rs` |
| 云同步 | 双引擎门面、打包解包、身份与指纹、索引与缓存、上传/取回/恢复编排 | `src/sync/`(见 [cloud-sync.md](cloud-sync.md) §1) |
| 凭据 | 三级存储与挑选顺序;明文文件;主密码加密文件;Linux 密钥环 | `src/secrets/{mod,plain,encrypted}.rs`、`src/secrets/keyring/` |
| GUI | Slint 视图 + 状态→属性 + 回调→消息 + 消息循环 | `src/ui/`(见 [gui.md](gui.md)) |
| wine | prefix 探测与选择、存档路径解析与反解析、`wineserver -k` | `src/wine.rs` |
| wine prefix 登记 | 记住 kotori 碰过的每个 prefix,信号路径兜底收尾 | `src/wine_prefixes.rs` |
| 进程 | 进程表快照、进程树、按名字找、`comm` 15 字节截断;unix 与 windows 各一套 | `src/process/{mod,unix,windows}.rs` |
| 显示器 | 输出分辨率探测(niri → KDE),`KOTORI_OUTPUT_RESOLUTION` 覆盖 | `src/display/mod.rs` |
| 外置盘 | 挂载表读写与「盘号 + 盘内相对目录」解析 | `src/mount/{mod,linux}.rs` |
| 文件对话框 | 借系统自己的框(portal / Windows shell)挑一个路径 | `src/picker.rs` |
| 平台探测 | 「这台机器上有什么」:逐项依赖探测,喂给设置页的环境检查 | `src/platform/{mod,probes,distro}.rs` |
| 游戏管理 | 扫描、手动添加、ID 生成、exe 智能挑选 | `src/game/{mod,scan}.rs` |
| 工具 | `find_binary`、带超时重试的执行器 | `src/util/{mod,executor,exec}.rs` |
| 测试 | 真实 daemon 的端到端测试(假 rclone / 假 kopia / 假 secret-tool)与黑盒契约测试 | `tests/ipc_e2e/`、`tests/portable_ipc/` |

`build.rs` 只做一件事:用 `slint-build` 把 `src/ui/slint/app.slint`(及其 import)编成 Rust,风格 `fluent`,并打开 `with_debug_info`(给 UI 的几何断言用)。

每个源文件的职责写在它自己的文件头注释里,那是权威;这张表只是地图。想知道某个文件干什么:

```bash
for f in $(git ls-files 'src/**/*.rs'); do
    printf '%-46s %s\n' "$f" "$(grep -m1 '^//!' "$f" | sed 's|^//! *||')"
done
```

---

## 5. 测试与验证

CI(`.github/workflows/`)分三层:

| workflow | 做什么 |
|----------|--------|
| `ci.yml` | 日常守门:x86_64 与原生 aarch64 上各跑一遍同一套测试;并编出 Linux 与 ARM 的交付产物作为 artifact |
| `verify.yml` | 可复用验证:fmt、文件行数红线、clippy 零告警、全部测试,外加固定版本 kopia 与真实 rclone 的契约测试。`ci.yml` 与 `release.yml` 共用这一份 |
| `release.yml` | 打 tag 时构建三端产物并创建 Release,发布**依赖同 SHA 的 verify** |

最近一次成功的 CI(`36232556424`)实测:

| 目标 | 结果 |
|------|------|
| Linux 单测 | 522 passed / 5 ignored |
| Linux `tests/ipc_e2e` | 39 passed / 1 ignored |
| `tests/portable_ipc`(Linux 与 Windows 各一遍) | 13 passed / 2 ignored |
| Windows 单测 | 417 passed / 4 ignored |
| 真 kopia 仓库契约(固定 0.23.1) | 4 passed |
| 真 rclone 契约 | 2 passed |

Windows 上单测更少,是因为 unix-only 模块连同它们的测试整块不参与编译 —— 这是设计如此,不是漏测。

`scripts/test.sh` 会先剥掉 `DISPLAY` / `WAYLAND_DISPLAY` / `WAYLAND_SOCKET` 再跑测试,因为**无头才是需要被覆盖的场景**:Slint 的测试后端按线程注册,本机有 DISPLAY 时它会悄悄退回 winit 兜底,于是本地全绿、CI 全红。规矩是 CI 跑什么本地就跑什么,都从这一个脚本走。

---

## 6. 继续往下读

| 想知道 | 读 |
|--------|-----|
| 谁拥有配置/会话/凭据、IPC 的 wire 形状与全部方法、环境变量、退出与信号 | [process-and-ipc.md](process-and-ipc.md) |
| gamescope 参数、启动尺寸怎么算、运行时三条通道、缩放档、退出看门狗 | [scaling.md](scaling.md) |
| 双引擎布局、身份与配对、云端索引、取回/恢复语义、凭据存储 | [cloud-sync.md](cloud-sync.md) |
| 为什么是 Slint、分层、Elm 式 update、自动保存三条规矩、无头怎么验证界面 | [gui.md](gui.md) |
| 第一次配置 Backblaze B2 的分步操作(面向用户) | [backblaze-setup.md](backblaze-setup.md) |

**建议的读码顺序**:`src/main.rs` → `src/config/mod.rs` → `src/daemon/mod.rs` → `src/scale/gamescope.rs` → `src/sync/runner/mod.rs` → `src/ui/update/mod.rs`。这条路径能把「一次启动」从头走到尾,其余模块都是它沿途用到的工具。

**改代码前先看行数红线**:单个源文件尽量 500 行以内,600 是底线,`scripts/check-file-size.sh` 分两档守着,CI 只在越过硬线时红。新写入的功能要是把某个文件顶过软线,本次就拆。
