# 双进程模型与 IPC

**这篇讲什么**:kotori 的 UI/CLI 进程与 daemon 进程怎么分工、谁拥有什么、两者之间的
wire 形状与全部方法、配置写入的并发规矩、路径与外部依赖怎么注入、以及退出与信号。
**什么时候读它**:要加一个 RPC、要改配置持久化、要排查"界面显示的和实际不一致"的时候。

核对来源:`src/daemon/{mod,protocol,game_rpc,scale_rpc,status_rpc,sync_rpc}.rs`、
`src/rpc.rs`、`src/config/paths.rs`、`src/main.rs`、`src/ui/{tasks,app,update}.rs`。

---

## 1. 谁启动谁

- **UI 与 CLI 都可以把 daemon 拉起来**:`daemon::ensure_running`(`src/daemon/mod.rs`)先
  `UnixStream::connect` 探一下;连不上就用 `current_exe()` + 参数 `daemon` spawn 一个,
  `stdin` 为 null,stdout/stderr 追加到 `<data_dir>/logs/daemon.log`,并且
  **`process_group(0)` 让它自立门户**(否则 UI 收到信号时会把 daemon 一起带走)。
  之后最多等 50 × 100ms = 5s 直到 socket 可连。
- **daemon 不做 double-fork,也不自建会话**。它就是被拉起的那一个子进程,只是换了进程组。
- 调用 `ensure_running` 的地方:`src/main.rs` 的 `scale_cli` / `sync_cli`、
  `src/game/mod.rs::launch`、`src/ui/mod.rs::run`、`src/ui/tasks.rs::connect_and_load`。
- **UI 是唯一会"先探测再决定要不要拉起"的调用方**:用户按过「停止服务」之后
  (`App::daemon_paused`),刷新与退避重试都改走 `load_without_booting`,`ensure_running`
  不再被调用。别把这条逻辑合并回去。

---

## 2. 所有权清单

| 资源 | 所有者 | 为什么 |
|------|--------|--------|
| `config.toml` 的写 | daemon | 只有一份内存副本;UI 直接写盘会出现两个写者 |
| 运行中会话表 | daemon | 曾经 daemon 里另存一份副本,结果它过期后把已退出的游戏报成"运行中" |
| 凭据句柄 / 主密码解锁状态 | daemon | 解锁状态是进程内的密钥,不可能跨进程共享 |
| rclone 子进程与其环境 | daemon | 凭据只经子进程环境传递,不进 argv、不落盘 |
| 窗口/页签/输入框内容 | UI 进程 | Slint 的 `in-out` 属性;Rust 只推不反向写(见 GUI 篇 §5) |
| 游戏进程组 | daemon spawn 并收尾 | 进程组 id 记在会话里 |

UI 进程崩溃**不影响**正在跑的游戏与退出后上传 —— 这是双进程模型存在的全部理由。

---

## 3. wire 形状

- 传输:**Unix socket**,`SOCKET_STREAM`,**一行一条 JSON-RPC 2.0**。
- 路径解析 `config::resolve_socket`:`KOTORI_SOCKET` > `config.daemon.socket_path` >
  `dirs::runtime_dir()/kotori.sock`(通常是 `/run/user/<uid>/kotori.sock`)。
- 请求 / 应答各一行,没有通知、没有批量、没有服务端主动推送。

```jsonc
// → 请求(客户端固定用 id = 1,见 src/rpc.rs)
{"jsonrpc":"2.0","id":1,"method":"game.update",
 "params":{"id":"demo","profile":{"name":"默认","algorithm":{"Fsr":{"sharpness":2}}}}}

// ← 成功
{"jsonrpc":"2.0","id":1,"result":{"success":true}}

// ← 失败
{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"missing parameter: id"}}
```

客户端的读法很窄:`src/rpc.rs::call` 只读**第一行**,有 `error` 就把 `message` 当错误
返回,否则取 `result`。所以每个方法都必须恰好回一行。

### 错误码

| code | 含义 | 出现处 |
|------|------|--------|
| `-32700` | JSON 解析失败 | `handle_request` |
| `-32600` | 不是 `"2.0"` | `handle_request` |
| `-32601` | 方法不存在 | `handle_request` 的兜底分支 |
| `-32602` | 参数缺失/形状不对 | `param_str`、`serde_json::from_value` |
| `-32000` | 处理器返回的业务错误(消息直接给用户看) | `protocol::respond` |

`params` 是**扁平对象**,不是数组。`game.update` 尤其要注意:字段与 `id` 平铺在
`params` 里,**没有 `patch` 这一层**;多包一层不会被 `GamePatch` 拒绝(它没有
`deny_unknown_fields`),而是静默忽略却回 `success: true`。

---

## 4. 方法清单

全部方法都在 `src/daemon/mod.rs::handle_request` 的一张大 `match` 里,实现分散在四个
`*_rpc.rs`。下表按域列出。

### 守护进程自身 / 配置

| 方法 | 参数 | 说明 |
|------|------|------|
| `daemon.status` | — | `running`、`games`(数量)、`sessions[]`(session_id / game_id / gamescope_pid / process_name / watch_only / elapsed_secs) |
| `daemon.shutdown` | — | 回包**先 flush 再**停循环;**不碰正在玩的游戏** |
| `config.reload` | — | 从 daemon 记住的那个路径重读配置 |
| `wine.status` | — | `configured` / `default` / `environment` / `detected[]` |
| `wine.set_prefix` | `prefix`:字符串或 `null` | 存在但缺 `drive_c` 时拒绝;不存在则接受(wine 会自己造) |
| `env.report` | — | 设置页「环境检查」:探测对外部程序的依赖(版本/路径、缺了会怎样、按发行版的安装命令)。探测**只读**,且会真去跑外部程序,所以只在设置页打开或点「重新检查」时调,不跟状态轮询。实现见 `src/platform/` |

### 游戏库与运行

| 方法 | 参数 | 说明 |
|------|------|------|
| `game.list` | — | **回传完整 `GameConfig`** + `id`,按 name 排序 |
| `game.create` | `name`、`exe_path`、可选 `game_dir` | 手动添加;exe 必须存在;ID 由名称生成 |
| `game.update` | `id` + 扁平补丁字段 | 唯一的设置持久化入口;`null` 表示清空可选字段 |
| `game.remove` | `id` | |
| `game.launch` | `id` | 先做启动前取回,再开会话;返回 `session_id`、`gamescope_pid`、`prefix_source`、`sync_pull` |
| `game.wait` | `session_id` | 阻塞到会话消失(`kotori launch` 用它) |
| `game.stop` | `session_id` | 收尾该会话 |

`game.update` 的补丁字段(`protocol::GamePatch`):`name`、`game_dir`、`exe_path`、
`launch_args`、`save_paths`、`wine_prefix`、`watch_only`、`process_name`、`profile`。
其中 `wine_prefix` / `process_name` 是 `Option<Option<T>>`(用 `double_option`),
所以"键缺失"与"显式 `null`"能区分开。

### 运行时缩放

| 方法 | 参数 | 说明 |
|------|------|------|
| `scale.get_status` | `session_id` | 启动时那份档案 + `live`(滤镜/锐度,读自 gamescope 的 X 属性) |
| `scale.toggle_fsr` / `scale.toggle_integer` | `session_id` | 先校验会话存在,再执行动作 |
| `scale.adjust_sharpness` | `session_id`、`delta` | 按 kotori 的方向步进,`|delta|` 上限 20 |
| `scale.action` | `session_id`、`action` | 动作 id 来自 `ScaleAction::from_id`,共 11 个 |

**⚠ 语义陷阱**:这几个方法都只把 `session_id` 用来"确认有这么个会话",真正的执行是
`engine.apply_action(action)`,它**对所有存活会话生效**(`src/scale/gamescope.rs`)。
两个游戏同时跑时,一条命令会同时改两个。

### 云同步

| 方法 | 参数 | 说明 |
|------|------|------|
| `sync.status` | — | 设置、rclone 路径、**当前生效的凭据级别**、存在的密钥名(**从不回声密钥值**)、每局游戏的存档位置数与上次结果 |
| `sync.set_settings` | `enabled`/`endpoint`/`bucket`/`prefix`/`encryption`/`keep_versions`/`force` | 改加密开关必须带 `force` |
| `sync.set_credentials` | `key_id`、`app_key` | 两个都空 = 清除;只填一个 = 拒绝 |
| `sync.set_password` | `password`、`force` | 存两种形态(明文 + rclone obscure 形态) |
| `sync.unlock` / `sync.lock` | `password` / — | 只对主密码文件后端有意义;别把"锁定"当"没存过" |
| `sync.set_master_password` | `password`、`force` | 把现有凭据封进主密码文件 |
| `sync.clear_master_password` | — | **不需要先解锁**(忘了主密码时的唯一出路) |
| `sync.test` | — | `rclone mkdir <remote_root>`,一次练到凭据+bucket+写权限 |
| `sync.now` | 可选 `id` | `id` 缺省 = 所有配了存档位置的游戏 |
| `sync.versions` | `id` | 云端快照名,最旧在前 |
| `sync.restore` | `id`、可选 `version` | 不带 `version` = 恢复最新;带则叠加那一份快照 |

---

## 5. 并发与配置写入

**连接层**(`handle_client`):每条连接一个 tokio task,循环"读一行 → `await`
`handle_request` → 写一行"。**同一条连接上的请求是串行的**,不同连接之间并发。

**配置写入**(`Daemon::mutate_config`)是唯一允许的写法:

```
拿写锁 → clone 出 candidate → 在 candidate 上改 → save_to(记住的那个路径)
       → 成功才 *guard = candidate;失败则内存不动
```

三条推论:

1. 内存视图与磁盘**不会分叉**:落盘失败时内存保持旧值。
2. daemon 记住的是**它启动时那个配置路径**(`config_path` 字段),不是每次重新解析
   —— 一次写不可能落到另一个文件上。
3. 但**读改写的原子性只到这一步**:`game.update` 收整份 `profile`,两个并发的
   `game.update` 会串行地互相覆盖(后一个的旧快照胜出)。这正是 UI 单游戏页
   "同一时刻只允许一笔写"存在的原因,见 [architecture-gui.md](architecture-gui.md) §4。

`config.reload` 是例外:它直接 `*self.config.write().await = 新读的`,会覆盖内存里尚未
落盘的改动,但它自己不做落盘。

---

## 6. 路径与外部依赖注入

所有路径都过 `src/config/paths.rs`(ADR-006),集成测试因此能起**真实 daemon**。

| 环境变量 | 作用 | 默认 |
|----------|------|------|
| `KOTORI_CONFIG` | `config.toml` 路径 | `~/.config/kotori/config.toml` |
| `KOTORI_DATA_DIR` | 数据/日志目录 | `~/.local/share/kotori`(日志在其 `logs/`) |
| `KOTORI_SOCKET` | daemon socket | 配置里的 `daemon.socket_path` → `$XDG_RUNTIME_DIR/kotori.sock` |
| `KOTORI_SECRETS_FILE` | 主密码凭据文件路径;**它的目录同时也是明文凭据文件的目录** | `<config 目录>/secrets.json` 与 `<同目录>/credentials.json` |
| `KOTORI_SECRET_TOOL` | `secret-tool` 可执行文件 | `which secret-tool`(仍要过"真探一次"的探测) |
| `KOTORI_RCLONE` | rclone 可执行文件 | `which rclone` |
| `KOTORI_WINESERVER` | `wineserver` 可执行文件 | `wineserver` |
| `KOTORI_OUTPUT_RESOLUTION` | `WxH`,覆盖显示器探测 | niri → KDE 探测 |
| `KOTORI_UI_SNAPSHOT` / `_SNAPSHOT_DELAY` / `_SEED_DELAY` / `_TAB` / `_SELECT` / `_SEARCH` / `_PICK` | 无显示器时验证 UI(仅 debug 构建) | 见 GUI 篇 §6 |
| `WINEPREFIX` | 参与 prefix 选择(优先级低于游戏档案与全局配置) | 见 `wine::resolve_prefix` |
| `ENABLE_GAMESCOPE_WSI` | 已设则**原样尊重**;未设时 kotori 注入 `0` | 见 scaling 篇 §2 |

`find_binary` 全部走 `src/util/executor.rs`,所以测试能塞假二进制。

---

## 7. 退出与信号

daemon 的主循环(`src/daemon/mod.rs::run`)是一个 `tokio::select!`,四个分支:

| 分支 | 触发 | 收尾行为 |
|------|------|----------|
| `listener.accept()` | 新连接 | 新 task 处理;accept 出错睡 50ms 重试 |
| `shutdown.notified()` | 收到 `daemon.shutdown` | **只停自己**;正在跑的游戏继续跑,`signalled = false` |
| `sigterm.recv()` | 关机 / 注销 | `signalled = true` → `close_all_sessions()` |
| `sigint.recv()` | Ctrl-C | 同上 |

退出时**一定**会做:`drop(listener)` + 删掉 socket 文件。信号路径额外做
`close_all_sessions()`,它对每个会话调 `engine.stop_session`,而 `stop_session` 本身
已经覆盖三层收尾(进程组、子进程树、`wineserver -k`)。

`close_all_sessions` **不动 `watch_only` 的会话的 prefix**:它只丢会话
(`process_group` 是 `None`),因为那是用户自己起的游戏,不归 kotori 关。

**启动时**:`run()` 在 bind 之前无条件 `remove_file(socket_path)`。这条对正常启动是
必要的(清理上次崩溃留下的陈旧 socket),但同时意味着**第二个 daemon 会抢走第一个的
socket 文件** —— 第一个仍在跑、仍有会话,只是再也没有人能找到它。`tests/ipc_e2e.rs`
里有一条专门覆盖这个行为的测试(`second_daemon_replaces_a_stale_socket_file`)。

**客户端侧**没有任何信号处理:`kotori launch` 收到的 Ctrl-C 只会终止 CLI 自己,
游戏与 daemon 都留着(这正是双进程想要的)。`src/rpc.rs` 也**不设客户端超时**,因为
`game.wait` 合法地会挂很久。

`TODO(未核对)`:systemd scope 与 90s `TimeoutStopSec` 的实际行为来自真机日志(见
`src/wine.rs` 与 `src/daemon/mod.rs` 的注释),本次只在代码里核对了 kotori 侧的动作,
没有在真机上复现一次关机。
