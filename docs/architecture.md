# Kotori 架构总览

**这篇讲什么**:kotori 是什么、边界在哪、两个进程各自拥有什么、一局游戏从点击到退出上传
完整走过哪些步骤,以及源码的模块地图。**什么时候读它**:第一次打开这个仓库,或者需要确认
"某件事该归谁管"的时候。往下钻的细节在文末索引的四篇里。

结论都来自 `src/**`、`Cargo.toml`、`build.rs`、`tests/`。凡是在代码里核对不到的,标
`TODO(未核对)`,不猜。

---

## 1. 定位与边界

kotori 是一个跨平台的 Galgame 管理器,但两个平台的能力**不对等**(`src/main.rs` 里的
`Cli` 子命令就是完整能力面):

| 平台 | 启动游戏 | gamescope 缩放增强 | 云存档同步 |
|------|----------|--------------------|------------|
| Linux x86_64(当前开发/交付目标) | ✅ | ✅ | ✅ |
| Windows x86_64(同步版,**未开始**) | ❌ | ❌ | ✅(计划) |

边界上还有几条硬规矩,写在代码注释与工具函数里:

- **一切启动都必须经 IPC 交给 daemon**。GUI 走 `src/ui/tasks.rs`,`CLI` 走
  `src/game/mod.rs::launch`,两者都先 `daemon::ensure_running` 再发 `game.launch`。
  没有任何一条路径在 UI/CLI 进程里直接 spawn gamescope。
- **`~/.config/kotori/config.toml` 是用户本地数据**,里面的游戏清单、挂载点、存档路径
  只出现在用户机器上;文档与测试里只用占位路径(`/games/demo/game.exe`)。
- **凭据绝不进 `config.toml`**,只在 `src/secrets/` 管的四个地方之一(见
  [architecture-sync.md](architecture-sync.md) §6)。
- **当前代码只有 Unix 能编译**:`tokio::net::UnixStream`(`src/daemon/mod.rs`、
  `src/rpc.rs`)与 `libc::process_group` / `libc::kill`(`src/scale/teardown.rs`、
  `src/daemon/mod.rs`)没有平台抽象层。Windows 版需要先补这一层。

技术栈:`Rust 2024` + `tokio` + `Slint 1.17`(GUI,`Cargo.toml`)+ 外部工具
`gamescope` / `wine` / `rclone`(都被包在可注入的查找函数后面,见第 4 节模块地图)。

---

## 2. 两个进程与所有权

```
┌───────────────────────────┐        Unix socket, 一行一条 JSON-RPC 2.0
│ kotori ui   (Slint 窗口)  │  ────────────────────────────────────────┐
│ kotori <cli 子命令>       │  ◄────────────────────────────────────────┤
└───────────────────────────┘                                          │
        │ 只有 daemon 不在时才 spawn 它(ensure_running)                 ▼
        │                                              ┌──────────────────────────────┐
        └─────────────────────────────────────────────►│ kotori daemon                │
                                                       │  config(RwLock) 唯一写者     │
                                                       │  sessions(HashMap) 唯一真相  │
                                                       │  keyring handle 唯一持有者   │
                                                       └──────────┬───────────────────┘
                                                                  │ spawn + 进程组
                                                                  ▼
                                                    gamescope → wine → 游戏
```

| 东西 | 谁拥有 | 代码位置 |
|------|--------|----------|
| 配置文件(读 + 写) | **daemon 独占** | `daemon::Daemon::mutate_config`(先改副本、落盘、再提交内存) |
| 运行中的会话 | **daemon 独占,只有一份** | `scale::gamescope::GamescopeScaleEngine::sessions` |
| 凭据(密钥环/凭据文件句柄) | **daemon 独占** | `daemon::sync_rpc::SyncState` |
| 游戏进程 | daemon spawn,daemon 收尾 | `scale::gamescope`,收尾见 `scale::teardown` |
| 窗口/页签状态、正在编辑的草稿 | UI 进程 | `src/ui/app.rs`、`src/ui/slint/**` |
| 日志 | 各自写各自的文件 | UI → `<data_dir>/logs/ui.log`;daemon → `<data_dir>/logs/daemon.log` |

两条容易被写错的边界:

1. **`daemon.shutdown` 不碰正在玩的游戏**(`daemon.shutdown` 只 `notify_one()` 让 accept
   循环退出),**只有信号**(SIGTERM/SIGINT)才把游戏一并收尾 —— 见
   `src/daemon/mod.rs::run` 里的 `signalled` 分支与 §4 生命周期第 7 步。
2. **UI 不许"自愈"掉用户按下的「停止服务」**:`App::daemon_paused` 一旦立起,周期刷新与
   失败退避重试都走 `load_without_booting`,`ensure_running` 不再被调用
   (`src/ui/tasks.rs`、`src/ui/update.rs`)。

---

## 3. 一局游戏的完整生命周期

以 GUI 点「启动」为例(CLI `kotori launch <id>` 只有第 2、8 步不同:它额外等 `game.wait`)。

```
①点启动 ②game.launch ③启动前取回 ④spawn gamescope ⑤Started
   │         │            │              │             │
   │         │            │              │             └─► ⑥UI 每 3s 轮询 daemon.status
   │         │            │              └─► 会话入 sessions 表,CWD=游戏根目录,WINEPREFIX 显式注入
   │         │            └─► rclone copy --update(上限 30s,失败只报告不拦)
   │         └─► JSON-RPC over Unix socket
   └─► Message::Launch → Task::perform(rpc) → 回包 Message::LaunchDone

⑦游戏结束(三条路,见 scaling 篇 §6) ⑧收尾 ⑨Ended 事件 ⑩等 3s ⑪rclone copy --backup-dir
```

分步说明(括号内是代码位置):

| # | 步骤 | 关键点 |
|---|------|--------|
| 1 | UI 发 `Message::Launch(id)`,`launching` 置位 | `src/ui/update.rs`;效果由 `src/ui/driver.rs::spawn` 跑在 tokio 上 |
| 2 | `game.launch` → daemon | `src/ui/update.rs` 直接构造 params;CLI 走 `src/game/mod.rs::launch` |
| 3 | **启动前自动取回**(同步开启且有存档位置时) | `sync_pull_before_launch`:`copy --update`,预算 `PULL_TIMEOUT` 30s,超时也照常启动 |
| 4 | 解析 prefix → 组 gamescope 命令行 → spawn | `wine::resolve_prefix` → `scale::build_gamescope_args` → `process_group(0)` |
| 5 | 会话登记 + 广播 `SessionKind::Started` | `sessions` 表是"什么在跑"的唯一真相 |
| 6 | UI 每 `STATUS_POLL`(3s)拉 `daemon.status` | 列表因此显示"运行中";`watch_only` 会话也在里面 |
| 7 | **游戏结束** | 三条路径(叉号 / 游戏内部退出 / 信号),详见 [architecture-scaling.md](architecture-scaling.md) §6 |
| 8 | 收尾:进程组 + 子进程树,再 `wineserver -k` | `scale::teardown::terminate_session` + `wine::close_prefix` |
| 9 | watcher 移除会话并广播 `SessionKind::Ended` | `daemon::spawn_sync_events` 只认 `Ended` |
| 10 | 等 `SETTLE_DELAY`(3s) | 让 `wineserver` 把刚写的存档刷到盘上 |
| 11 | **退出后自动上传** | `sync_after_game_exit` → `Runner::upload`(`copy` + `--backup-dir` + 空 `--suffix`) |

失败面:`game.launch` 在 gamescope 300ms 内立刻退出时返回错误(`try_wait` 判定),UI 落在
顶部错误条;取回失败只写进回包的 `sync_pull` 字段与日志,**不拦启动**。

---

## 4. 模块地图

| 模块 | 负责什么 | 主要文件 |
|------|----------|----------|
| 入口 / CLI | 子命令分发、日志初始化、`scale`/`sync` 的共用汇报 | `src/main.rs`、`src/cli/mod.rs` |
| IPC 客户端 | 一行 JSON-RPC 发一条收一条 | `src/rpc.rs` |
| daemon | 分发请求、**唯一配置写者**、信号处理、会话事件 → 同步钩子 | `src/daemon/mod.rs` |
| daemon/*_rpc | 按域切开的处理器 + wire 结构 | `daemon/{game,scale,status,sync}_rpc.rs`、`protocol.rs` |
| 配置 | 类型、读写、路径(全部可注入)、缩放档案 | `src/config/{mod,paths,profile}.rs` |
| 缩放引擎 | gamescope 命令行、会话、运行时属性、退出看门狗 | `src/scale/{args,gamescope,x11,action,teardown}.rs` |
| 桌面集成 | 只做一件事:KDE 的 KWin 脚本桥(窗口尺寸/全屏) | `src/desktop/{mod,kde}.rs` |
| 云同步 | rclone 参数与快照策略 / 真正执行 | `src/sync/{mod,runner}.rs` |
| 凭据 | 四级存储与挑选顺序;明文文件;主密码加密文件 | `src/secrets/{mod,plain,encrypted}.rs` |
| GUI | Slint 视图 + 状态→属性 + 回调→消息 + 消息循环 | `src/ui/**`(详见 [architecture-gui.md](architecture-gui.md)) |
| wine | prefix 探测与选择、存档路径解析与反解析、`wineserver -k` | `src/wine.rs` |
| 进程探测 | `/proc` 轮询、进程树、`comm` 15 字节截断 | `src/process.rs` |
| 显示器 | 输出分辨率探测(niri → KDE),`KOTORI_OUTPUT_RESOLUTION` 覆盖 | `src/display.rs` |
| 文件对话框 | 借系统自己的框(portal / Windows shell)挑一个路径 | `src/picker.rs` |
| 平台探测 | 「这台机器上有什么」:逐项依赖探测(版本/路径、缺了会怎样、按发行版的安装命令),喂给设置页的「环境检查」 | `src/platform/mod.rs` |
| 游戏管理 | 扫描、手动添加、ID 生成、exe 智能挑选 | `src/game/mod.rs` |
| 工具 | `find_binary`、带超时重试的 `execute_with_timeout` | `src/util/{mod,executor}.rs` |
| 测试 | 真实 daemon 的端到端测试(假 rclone / 假显示器 / 假 secret-tool) | `tests/ipc_e2e.rs` |

`build.rs` 只做一件事:用 `slint-build` 把 `src/ui/slint/app.slint`(及其 import)编成
Rust,风格 `fluent`,并打开 `with_debug_info`(给 UI 的几何断言用)。

---

## 5. 继续往下读

| 想知道 | 读 |
|--------|-----|
| 谁拥有配置/会话/凭据、IPC 长什么样、方法清单、环境变量、退出与信号 | [architecture-process.md](architecture-process.md) |
| gamescope 参数、启动时怎么算尺寸、运行时三条通道、缩放档、退出看门狗 | [architecture-scaling.md](architecture-scaling.md) |
| rclone 布局、取回/恢复语义、快照滚动窗口、凭据四级存储 | [architecture-sync.md](architecture-sync.md) |
| 为什么是 Slint、四层分工、Elm 式 `update`、自动保存三条规矩、没显示器时怎么验证 | [architecture-gui.md](architecture-gui.md) |
| 第一次上手 B2 的分步操作 | [cloud-sync.md](cloud-sync.md)(面向用户,不是架构文档) |

**建议的读码顺序**:`src/main.rs` → `src/config/mod.rs` → `src/daemon/mod.rs` →
`src/scale/gamescope.rs` → `src/sync/runner.rs` → `src/ui/update.rs`。这条路径能把
"一次启动"从头走到尾,其余模块都是它沿途用到的工具。

`TODO(未核对)`:本仓库没有可用的 aarch64 std/rustup,Windows 目标也不在当前工具链里,
所以"除 Unix 外是否真的编不过"只由代码里的 `UnixStream` / `libc` 用法推断,没有实测。
