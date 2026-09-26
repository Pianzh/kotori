# 双进程模型与 IPC

**这篇讲什么**:UI/CLI 进程与 daemon 进程怎么分工、谁拥有什么、两者之间的 wire 形状与全部方法、配置写入的并发规矩、路径与外部依赖怎么注入、以及退出与信号。

**什么时候读它**:要加一个 RPC；要改配置持久化；要排查「界面显示的和实际不一致」。

核对来源:`src/daemon/{mod,dispatch,ipc,protocol,game_rpc,scale_rpc,status_rpc}.rs`、`src/daemon/sync_rpc/`、`src/rpc.rs`、`src/config/paths.rs`、`src/main.rs`、`src/ui/{mod,task,app,update/}`。

---

## 1. 谁启动谁

- **UI 与 CLI 都可以把 daemon 拉起来**。`daemon::ensure_running` 先探一下端点 —— Unix 是 `UnixStream::connect`,Windows 是命名管道的 open;连不上就用 `current_exe()` + 参数 `daemon` spawn 一个,stdin 为 null,stdout/stderr 追加到 `<data_dir>/logs/daemon.log`。之后最多等 50 × 100ms = 5s,直到端点可连。
- 启动方式分平台:Unix 用 **`process_group(0)`** 让它自立门户(否则 UI 收到信号时会把 daemon 一起带走);Windows 没有进程组概念,改用 **`CREATE_NO_WINDOW`**,别让后台进程弹黑框。
- **daemon 不做 double-fork,也不自建会话**。它就是被拉起的那一个子进程,只是换了进程组。
- 调用 `ensure_running` 的地方:`src/main.rs` 的 scale/sync 子命令、`src/game/mod.rs::launch`、`src/ui/mod.rs::run`、`src/ui/task.rs`。
- **UI 是唯一会「先探测再决定要不要拉起」的调用方**。用户按过「停止服务」之后(`App::daemon_paused`),刷新与退避重试都改走 `load_without_booting`,`ensure_running` 不再被调用。别把这条逻辑合并回去。

---

## 2. 所有权清单

| 资源 | 所有者 | 为什么 |
|------|--------|--------|
| `config.toml` 的写 | daemon | 只有一份内存副本;UI 直接写盘会出现两个写者 |
| 运行中会话表 | daemon | 曾经 daemon 里另存一份副本,结果它过期后把已退出的游戏报成「运行中」 |
| 凭据句柄 / 主密码解锁状态 | daemon | 解锁状态是进程内的密钥,不可能跨进程共享 |
| kopia / rclone 子进程与其环境 | daemon | 凭据不落盘:rclone 只经子进程环境;kopia 的仓库密码走环境变量,B2 key 只在建/连仓库那一次进 argv |
| 窗口/页签/输入框内容 | UI 进程 | Slint 的 `in-out` 属性;Rust 只推不反向写(见 [gui.md](gui.md) §5) |
| 游戏进程组 | daemon spawn 并收尾 | 进程组 id 记在会话里 |

UI 进程崩溃**不影响**正在跑的游戏与退出后上传 —— 这是双进程模型存在的全部理由。

---

## 3. wire 形状

- 传输按平台分,对上层只是「一个能读写的 tokio 流」:Unix 是 **Unix socket**,Windows 是**命名管道** `\\.\pipe\kotori-<用户名>`。管道名带用户,因为 `\\.\pipe\` 是全机器命名空间,不带后缀同一台机器上两个用户会互相抢。两边都是**一行一条 JSON-RPC 2.0**。
- 路径解析顺序:`KOTORI_SOCKET` > 配置里的 `daemon.socket_path` > 默认值(Unix 取 runtime dir 下的 `kotori.sock`;Windows 上那个「路径」直接就是管道名,`PathBuf` 只当字符串容器)。
- **命名管道的形状差异**:服务端必须**预建**实例,否则「上一个实例刚被连上、下一个还没建出来」的空档里,客户端 `open()` 会撞 `ERROR_PIPE_BUSY`(231,Windows VM 实测发作过)。做法是先建出下一个实例再拿手里的那个去 `connect`,客户端对 231 做短重试兜底。
- 请求 / 应答各一行。**没有通知、没有批量、没有服务端主动推送**。

```jsonc
// → 请求(客户端固定用 id = 1)
{"jsonrpc":"2.0","id":1,"method":"game.update",
 "params":{"id":"demo","name":"demo"}}

// ← 成功
{"jsonrpc":"2.0","id":1,"result":{"success":true}}

// ← 失败
{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"missing parameter: id"}}
```

客户端的读法很窄:`rpc::call` 只读**第一行**,有 `error` 就把 `message` 当错误返回,否则取 `result`。所以每个方法都必须恰好回一行。

### 错误码

| code | 含义 |
|------|------|
| `-32700` | JSON 解析失败 |
| `-32600` | 不是 `"2.0"` |
| `-32601` | 方法不存在(回的是 `method not found: xxx`) |
| `-32602` | 参数缺失或形状不对 |
| `-32000` | 处理器返回的业务错误,消息直接给用户看 |

`params` 是**扁平对象**,不是数组。`game.update` 尤其要注意:字段与 `id` 平铺在 `params` 里,**没有 `patch` 这一层**;多包一层不会被拒绝(那个结构没有 `deny_unknown_fields`),而是静默忽略却回 `success: true`。

---

## 4. 方法清单

全部 45 个方法都在 `src/daemon/dispatch.rs` 的一张大 `match` 里,实现分散在 `game_rpc.rs` / `game_write.rs` / `scale_rpc.rs` / `status_rpc.rs` 与 `sync_rpc/`。**加方法时这张表也要一起改。**

### daemon 自身 / 配置

| 方法 | 参数 | 说明 |
|------|------|------|
| `daemon.status` | — | `running`、`games`(数量)、`sessions[]`(session_id / game_id / pid / process_name / watch_only / elapsed_secs) |
| `daemon.shutdown` | — | 回包**先 flush 再**停循环;**不碰正在玩的游戏** |
| `config.reload` | — | 从 daemon 记住的那个路径重读配置 |
| `config.set_source` | 便携 / 默认 | 在便携配置与平台默认目录之间切换,让路的那份改名保留 |
| `process.list` | — | 正在运行的进程,喂给「跟随的进程」选择器 |
| `wine.status` | — | `configured` / `default` / `environment` / `detected[]` |
| `wine.set_prefix` | `prefix`:字符串或 `null` | 存在但缺 `drive_c` 时拒绝;不存在则接受(wine 会自己造) |
| `env.report` | — | 环境检查:探测对外部程序的依赖(版本/路径、缺了会怎样、按发行版的安装命令)。探测只读,且会真去跑外部程序,所以只在设置页打开或点「重新检查」时调,不跟状态轮询。清单按平台各算各的:Windows 上没有 gamescope / wine / 窗口尺寸控制这三项 |

### 游戏库与运行

| 方法 | 参数 | 说明 |
|------|------|------|
| `game.list` | — | 回传完整 `GameConfig` + `id`,按 name 排序 |
| `game.create` | `name`、`exe_path`、可选 `game_dir` | 手动添加;exe 必须存在;ID 由名称生成 |
| `game.update` | `id` + 扁平补丁字段 | 唯一的设置持久化入口;`null` 表示清空可选字段 |
| `game.remove` | `id` | |
| `game.add` | `directory` | 扫描目录并把结果批量写进配置 |
| `mount.infer` | `path` | 从一个绝对路径推出它属于哪块盘 + 盘内相对目录,只读不写 |
| `game.launch` | `id` | 启动前自检与取回,再开会话;返回 `session_id`、`pid`、`prefix_source`、`sync_pull` |
| `game.wait` | `session_id` | 阻塞到会话消失(`kotori launch` 用它) |
| `game.stop` | `session_id` | 收尾该会话 |

`game.update` 的补丁字段:`name`、`game_dir`、`exe_path`、`launch_args`、`save_paths`、`wine_prefix`、`watch_only`、`process_name`、`profile`。其中 `wine_prefix` / `process_name` 是 `Option<Option<T>>`(用 `double_option`),所以「键缺失」与「显式 `null`」能区分开。

**两端都能用的部分**:`game.launch` 在 Windows 上是**直接启动 exe**(不套 gamescope),`game.wait` / `game.stop` 照常工作,退出后上传照常发生。只有缩放那几项在那边回「做不到」。

### 运行时缩放

| 方法 | 参数 | 说明 |
|------|------|------|
| `scale.get_status` | `session_id` | 启动时那份档案 + `live`(滤镜/锐度,读自 gamescope 的 X 属性) |
| `scale.toggle_fsr` / `scale.toggle_integer` | `session_id` | 先校验会话存在,再执行动作 |
| `scale.adjust_sharpness` | `session_id`、`delta` | 按 kotori 的方向步进,`abs(delta)` 上限 20 |
| `scale.action` | `session_id`、`action` | 动作 id 来自 `ScaleAction::from_id`,共 11 个 |

**⚠ 语义陷阱**:这几个方法都只把 `session_id` 用来「确认有这么个会话」,真正的执行是 `engine.apply_action(action)`,它**对所有存活会话生效**。两个游戏同时跑时,一条命令会同时改两个。

**⚠ Windows 上这些方法如实回「做不到」**。`scale_rpc` 的动作分发在 Windows 上直接报错,刻意不给空后端补同名方法去凑合 —— 签名一样、返回 `Ok`,会让界面以为缩放生效了。`scale.get_status` 的 `live` 那一段是 Unix 专有。

### 云同步

| 方法 | 参数 | 说明 |
|------|------|------|
| `sync.status` | — | 设置、引擎选择、引擎二进制路径、**当前生效的凭据级别**、存在的密钥名(**从不回声密钥值**)、每款游戏的存档位置数与上次结果 |
| `sync.set_settings` | `enabled`/`engine`/`endpoint`/`bucket`/`prefix`/`keep_versions` | 无 `encryption` 字段 —— 加密是 kopia 引擎自带的;换引擎会返回 `engine_changed` 标志 |
| `sync.set_credentials` | `key_id`、`app_key` | 两个都空 = 清除;只填一个 = 拒绝 |
| `sync.set_kopia_password` | `password` | 留空 = 清除并回到默认密码;非空则存进凭据库,回 `using_default` 标志给 UI |
| `sync.unlock` / `sync.lock` | `password` / — | 只对主密码文件后端有意义;别把「锁定」当「没存过」 |
| `sync.set_master_password` | `password`、`force` | 把现有凭据封进主密码文件 |
| `sync.clear_master_password` | — | **不需要先解锁**(忘了主密码时的唯一出路) |
| `sync.test` | — | 按所选引擎验证:一次验到凭据 + bucket + 读写权限 |
| `sync.now` | 可选 `id` | `id` 缺省 = 所有配了存档位置的游戏 |
| `sync.versions` | `id` | 本机这款在云端的版本列表(最旧在前) |
| `sync.cloud_games` | — | 云端已有的全部游戏,含本机没装的 |
| `sync.cloud_list` | 可选 `refresh` | 云端列表。`refresh: true` 是「云端存档」页那颗刷新按钮(强制联网);不传就只在缓存超过一小时或本地没有缓存时才去云端 |
| `sync.cloud_versions` | `key` | 某一款在云端的版本。参数是**云端落点**,不是本机 id —— 云端有而本机没有的游戏也要能列 |
| `sync.match` | `exe` | 添加游戏时那一问:这个 exe 在云端是哪一款。参数是本机路径,指纹由 daemon 自己算 |
| `sync.resolve` | `id`、`choice`、可选 `cloud_id`/`cloud_key` | 回答启动前那一问:`ok` / `off` / `pair` |
| `sync.pairing` | — | 配对候选列表 |
| `sync.pair` | `id`、`cloud_key`、`cloud_id` | 绑定本机档案到云端身份 |
| `sync.reject` | `id`、`cloud_id` | 记下「不是同一款」,之后不再自动绑回来 |
| `sync.delete_version` | `key`、`version` | 删云端某一版。先列一次确认,删一个不存在的版本名要明确报错 |
| `sync.delete_versions` | `key` | 清空某一款的**全部存档**,词条留着 |
| `sync.delete_identity` | `key` | 删掉整条词条,连存档一起 |
| `sync.restore` | `id`、可选 `version` | 不带 `version` = 恢复最新;带则恢复指定版本;**直接覆盖**,不先做安全快照 —— 每一版本身就是完整的,回退到上一版就是撤销 |

三个删除方法都按**云端落点**收参数,和 `sync.cloud_versions` 同一把尺子,这样「云端有、本机没有」的游戏也清得掉。

---

## 5. 并发与配置写入

**连接层**:每条连接一个 tokio task,循环「读一行 → `await` 处理器 → 写一行」。**同一条连接上的请求是串行的**,不同连接之间并发。

**配置写入**(`Daemon::mutate_config`)是唯一允许的写法:

```
拿写锁 → clone 出 candidate → 在 candidate 上改 → 写回(记住的那个路径)
       → 成功才 *guard = candidate;失败则内存不动
```

三条推论:

1. 内存视图与磁盘**不会分叉**:落盘失败时内存保持旧值。
2. daemon 记住的是**它启动时那个配置路径**,不是每次重新解析 —— 一次写不可能落到另一个文件上。
3. 但**读改写的原子性只到这一步**。`game.update` 收整份 `profile`,两个并发的 `game.update` 会串行地互相覆盖(后一个的旧快照胜出)。这正是 UI 单游戏页「同一时刻只允许一笔写」存在的原因,见 [gui.md](gui.md) §4。

`config.reload` 是例外:它直接替换整个内存视图,会覆盖尚未落盘的改动,但自己不做落盘。

---

## 6. 路径与外部依赖注入

所有路径都过 `src/config/paths.rs`,集成测试因此能起**真实 daemon**。

| 环境变量 | 作用 | 默认 |
|----------|------|------|
| `KOTORI_CONFIG` | `config.toml` 路径 | 便携那份(与可执行文件同目录)优先,否则平台默认目录 |
| `KOTORI_DATA_DIR` | 数据/日志目录 | 平台数据目录(便携模式下跟着配置目录走) |
| `KOTORI_SOCKET` | daemon 端点 | 配置里的 `daemon.socket_path` → runtime dir 下的 `kotori.sock` / 命名管道 |
| `KOTORI_SECRETS_FILE` | 主密码凭据文件路径;**它的目录同时也是明文凭据文件的目录** | `<config 目录>/secrets.json` 与 `<同目录>/credentials.json` |
| `KOTORI_SECRET_TOOL` | `secret-tool` 可执行文件 | `which secret-tool`(仍要过「真探一次」的探测) |
| `KOTORI_RCLONE` | rclone 可执行文件 | `which rclone` |
| `KOTORI_KOPIA` | kopia 可执行文件 | `which kopia` |
| `KOTORI_KOPIA_REPOSITORY` | 把 kopia 仓库放到一个**本地目录**(NAS、挂载盘)而不是 B2 | 缺省 = 仓库直接落在 B2 的 `<prefix>/kopia` |
| `KOTORI_WINESERVER` | `wineserver` 可执行文件 | `wineserver` |
| `KOTORI_OUTPUT_RESOLUTION` | `WxH`,覆盖显示器探测 | niri → KDE 探测 |
| `KOTORI_RENDERER_RETRIED` | 内部用:标记已经退回过一次软件渲染,防止无限重启 | 未设 |
| `KOTORI_TEST` | 内部用:测试夹具开关 | 未设 |
| `KOTORI_UI_SNAPSHOT` / `_SNAPSHOT_DELAY` / `_SEED_DELAY` / `_TAB` / `_SELECT` / `_SEARCH` / `_PICK` | 无显示器时验证 UI(仅 debug 构建) | 见 [gui.md](gui.md) §6 |
| `WINEPREFIX` | 参与 prefix 选择(优先级低于游戏档案与全局配置) | 见 `wine::resolve_prefix` |
| `ENABLE_GAMESCOPE_WSI` | 已设则**原样尊重**;未设时 kotori 注入 `0` | 见 [scaling.md](scaling.md) §2 |

外部程序的查找顺序(`sync::executables::find`):设置页里填的位置 > 对应的 `KOTORI_*` 变量 > **kotori 可执行文件旁边** > `PATH`。前三步都要求那里真的有那个文件,填错不会静默退到 PATH。同步引擎的查找走 `src/sync/executables.rs`,`find_binary` 走 `src/util/executor.rs`,所以测试都能塞假二进制。

---

## 7. 退出与信号

daemon 的主循环是一个 `tokio::select!`,三个分支:

| 分支 | 触发 | 收尾行为 |
|------|------|----------|
| `listener.accept()` | 新连接 | 新 task 处理;accept 出错睡 50ms 重试 |
| `shutdown.notified()` | 收到 `daemon.shutdown` | **只停自己**;正在跑的游戏继续跑 |
| `session_end_signal()` | Unix:SIGTERM(关机/注销)或 SIGINT(终端 Ctrl-C);Windows:控制台 Ctrl-C | 关掉全部会话 |

退出时**一定**会做:丢掉 listener + 删掉 socket 文件(Windows 上没有文件可删,管道由内核持有,最后一个实例关掉它自己就没了)。信号路径额外关掉全部会话,最后再收一遍「没人认领」的残留 prefix —— 上一次 daemon 被 SIGKILL 掉时留下的那局,`winedevice.exe` 无视 SIGTERM、又不在任何我们能杀的进程组或进程树里,只有 `wineserver -k` 收得掉它(它就是 90s 关机的元凶)。收尾时 gamescope 常以 SIGABRT 结束,KDE 于是弹崩溃通知。

**关全部会话时不动观测会话的 prefix**:那不是我们启动的游戏,不归 kotori 关。

**单实例锁**在启动时取(Unix 是对锁文件取非阻塞 `flock`,Windows 是独占打开)。从前的写法是 bind 前无条件删掉 socket 文件 —— 那意味着**第二个 daemon 会抢走第一个的 socket 文件**,而两个写者同时写 `config.toml` 直接违背「daemon 是唯一配置写者」。现在两个行为分开:活着的被挡(第一个毫发无损),死文件照旧清掉重绑,两条各有测试守着。

**客户端侧没有任何信号处理**:`kotori launch` 收到的 Ctrl-C 只会终止 CLI 自己,游戏与 daemon 都留着(这正是双进程想要的)。客户端也**不设超时**,因为 `game.wait` 合法地会挂很久。

没有实测过的部分:关机时 systemd scope 与 `TimeoutStopSec` 的实际行为来自真机日志,代码侧的动作是核过的,但没有在真机上复现一次完整关机。
