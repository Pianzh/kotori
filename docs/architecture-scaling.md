# 缩放引擎

**这篇讲什么**:Linux 上 kotori 怎么用 gamescope 把一局游戏画大、启动时窗口尺寸从哪来、
运行中改缩放走哪三条通道(以及为什么一条都不注入按键)、缩放档次的算术、以及一局结束后
怎么把进程真的收干净。Windows 上 kotori 不做缩放(缩放归外部工具 Magpie,观察后端还没写),
`scale_rpc` 对动作统一回"这个平台做不到",能力表据此把缩放编辑设为**不适用**(不是"缺失")。
**什么时候读它**:动 gamescope 参数、动运行时缩放、或者排查"退了游戏
还有残留进程 / 关机卡住"的时候。

核对来源:`src/scale/{args,gamescope,action,x11,teardown,unsupported}.rs`、
`src/daemon/scale_rpc.rs`、`src/desktop/kde.rs`、`src/config/profile.rs`、
`src/display.rs`、`src/wine.rs`、`src/process.rs`。

---

## 1. gamescope 参数模型(>= 3.16)

参数只在 `src/scale/args.rs::build_gamescope_args` 里生成,那是一个纯函数(屏幕尺寸由
调用方传入),它的单测就是这份契约。

| 参数 | 含义 | kotori 怎么用 |
|------|------|---------------|
| `-w` / `-h` | 游戏自己的渲染分辨率 | **只有档案显式填了**才发(两半都填才算) |
| `-W` / `-H` | 嵌套窗口的**初始**输出尺寸(物理像素) | 总是发:`profile.output_size_for(screen)` |
| `-S <scaler>` | `auto`/`integer`/`fit`/`fill`/`stretch` | Fsr/Nis/Bilinear → `fit`;Integer → `integer` |
| `-F <filter>` | `linear`/`nearest`/`fsr`/`nis`/`pixel` | 与算法一一对应 |
| `--sharpness <0..20>` | gamescope 自己的刻度:**0 最锐、20 最柔** | 仅 Fsr/Nis 发 |
| `-r <fps>` | 帧率上限 | 仅 `framerate_limit` 有值时发 |
| `-f` | 把窗口钉满输出 | 仅 `force_fullscreen` 为真时发;**默认关** |
| `--` | 分隔符 | 之后是 `/usr/bin/wine <exe> <launch_args…>` |

三条**不能碰**的历史坑(都有回归测试守着):**`-s` 现在是 `--mouse-sensitivity`**,而且会
吞掉下一个参数,绝不是 FSR;**`--fsr-sharpness`** 只是 `--sharpness` 的别名,老代码用了反的
极性;kotori 内部锐度是 `0..=5`(越大越锐),换算只有一处 ——
`sharpness_to_gamescope(sharpness) = 20 - min(sharpness, 5) * 4`。

---

## 2. 启动:窗口尺寸是怎么算出来的

```
config.toml 的 scale_profile
  ├─ explicit_internal_size()  internal_width+internal_height 都 >0 才算 ⇒ 有值才发 -w/-h
  ├─ explicit_output_size()    scale_ratio 优先(有限且 >0),否则那一对 output_*(也要两半齐)
  └─ output_size_for(screen)   = explicit_output_size() ?? screen ⇒ -W/-H 总是发
```

`screen` 由 `scale::gamescope::screen_size()` 给:`display::primary_resolution_or((1920, 1080))`,
而 `display::primary_resolution` 的顺序是 `KOTORI_OUTPUT_RESOLUTION` → niri focused-output →
niri 最大 output → KDE(`kscreen-doctor -j`)→ `None`。

**关键语义**:

- **两样都留空 = 开一个屏幕上那么大的普通窗口**。这不是"全屏",也不是"未知":
  `-W/-H` 只是**初始尺寸**,合成器随时可以改,窗口可以拖小。
- **`-w/-h` 留空 ≠ 未知**。gamescope 自己的默认值是 1280×720;不传这两个参数时它画的
  和传 `-w 1280 -h 720` 完全一样。差别只在配置里:那一对数字不会再被假装成"用户选过"。
  需要算术(算比例、算窗口尺寸)时用 `ScaleProfile::internal_size()` 兜底取 1280×720。
- **`force_fullscreen` 默认 false**。`-f` 会把 `g_nOutput` 钉成屏幕几何,配置的比例白设、
  用户也拖不小 —— 而"启动即最大化"这个需求 `-W/-H` 已经满足了。
- **`follow_window` 是死字段**:`build_gamescope_args` 明确不让它出现在命令行上(有测试
  `follow_window_is_not_a_command_line_flag`),运行时也没有任何地方读它。硬件锁窗口尺寸
  需要 KWin 窗口规则,没做。

### 会话是怎么起出来的

`GamescopeScaleEngine::start_session`(`src/scale/gamescope.rs`)按顺序:
`find_binary` 找 gamescope 与 wine(任一缺失即报错)→ `current_dir(游戏根目录)`
(`effective_game_dir()`,不是 exe 的上级目录)→ `process_group(0)`(gamescope 当进程组
组长,收尾才能整组杀)→ **显式注入 `WINEPREFIX`**(不注入就会落到 `~/.wine`)→
`ENABLE_GAMESCOPE_WSI`:用户已设就尊重,未设则注入 `"0"`(关掉那个 Vulkan WSI 层,它在
nested Wayland + NVIDIA 上会崩)→ spawn 后睡 300ms 再 `try_wait()`(立刻退出 ⇒ 报
`GamescopeStartFailed` 并把命令行带进错误消息)→ 登记会话(含 `output_size`、
`runtime_ratio`、`process_group`、`wine_prefix`)、广播 `SessionKind::Started`,再起三个
后台 task:watcher、退出看门狗、游戏进程探针(§5)。

`watch_only` 的会话走另一条路(`start_watch_session`):**什么都不启动**,只轮询
`process_name`(先等它出现,`process::APPEAR_TIMEOUT` 300s,再等它消失),
`gamescope_pid` / `process_group` / `wine_prefix` 全是 `None`。

---

## 3. 运行时缩放:三条通道

`ScaleAction` 一共 11 个 id(`src/scale/action.rs::id`),`is_filter()` 把它们分成两半。

### 通道 A —— 滤镜 / 锐度 / scaler:gamescope 自己的 X 根窗口属性

`src/scale/x11.rs` 是全项目**唯一**说 X11 的地方。理由写在文件头:gamescope 没有控制
协议(`gamescope_control` v7 只有截图/刷新率/LUT/键盘布局),而它**自己**在
`steamcompmgr` 里盯着这些属性:

| 属性 | 取值 |
|------|------|
| `GAMESCOPE_NEW_SCALING_FILTER` | `GamescopeUpscaleFilter`:`Linear=0 Nearest=1 Fsr=2 Nis=3 Pixel=4` |
| `GAMESCOPE_NEW_SCALING_SCALER` | `GamescopeUpscaleScaler`:`Auto=0 Integer=1 Fit=2 Fill=3 Stretch=4` |
| `GAMESCOPE_FSR_SHARPNESS` | `0..20`(0 最锐) |
| `GAMESCOPE_PID` | gamescope 自己写进去的 pid,**用来认准实例** |

要点:

- **怎么找到那个 X 服务器**:扫 `/tmp/.X11-unix/X<N>`,逐个连 `:<N>`,读 `GAMESCOPE_PID`
  比对。区分 `Ok(None)`(没有)与 `Err`(连不上)是刻意的 —— 两者对用户是不同的事。
- **写入顺序**:scaler → sharpness → filter,filter 放最后(每次改都会触发重绘,放最后
  才不会出现中间帧)。
- **读回来的是我们自己上次写的东西**:gamescope 从不回写这些属性,所以"切换"没有查询 API,
  靠读自己的上一笔;新会话读不到时回退到 `Settings::for_algorithm(算法)`(那份档案就是它的
  启动参数)。因此 `Settings::for_algorithm` 必须与 `build_gamescope_args` 保持一致
  —— 一个是启动时传的,一个是运行时推的,有测试盯着。
- 未知的 filter 值会**报错**(`UnknownFilter`),不猜;未知 scaler 值退化成 `Auto`。

### 通道 B —— 窗口尺寸 / 全屏:KWin 脚本

`src/desktop/kde.rs`。之所以必须走合成器:运行中的 gamescope 是合成器的 **Wayland 客户端**,
Wayland 不允许一个客户端改另一个客户端的窗口大小。

- 只认 **KDE**:`desktop::is_kde()` 看 `XDG_CURRENT_DESKTOP` 含 "kde";其他桌面直接返回
  `ApplyError::Unsupported("窗口缩放与全屏暂只在 KDE 上实现…")`。niri 是平铺的,窗口
  尺寸归布局管。
- 实现方式是 `org.kde.KWin` / `/Scripting` 的 `loadScript(path, plugin)` + `Script.run`。
  **插件名固定**(`kotori-scale` / `kotori-fullscreen`),每次先 `unloadScript` 再
  `loadScript`,否则一晚上会在会话里攒下上百个脚本。脚本落在 `<data_dir>/kwin/<plugin>.js`。
- 传给脚本的是**物理像素**;脚本自己除以 `w.output.devicePixelRatio`,并用
  `w.output.geometry` 夹在屏内(不是 `workspace.clientArea` —— 它拒绝所有参数组合)。
  几何赋值必须**拷贝已有的对象**:
  `w.frameGeometry = Object.assign({}, w.frameGeometry, {width, height})`,新字面量会被
  静默忽略。
- **赋值是异步的**:赋完立刻回读还是旧尺寸,所以**不能用回读几何自证成功**。
  ⚠ 由此 `resize_window()` 返回 `Ok` **什么都不证明** —— 它只看 D-Bus 调用是否成功,
  脚本内部"没有这个 pid 的窗口"那条分支照样成功返回。要拿它当证据,得让脚本把结果回传
  (`print()` 会进 journal,能读)。

全屏走的是 `w.fullScreen = !w.fullScreen`:方向由 KWin 决定并记住,kotori 不存副本,
所以按两次一定回到起点。

### 通道 C —— 为什么不再有全局快捷键

**kotori 现在不注册、也不注入任何按键**(整套 portal `GlobalShortcuts` 注册已删除)。
理由是三条都成立:窗口那一半需要合成器权限(portal 给不了);滤镜那一半走 X 属性,本来就
不需要键和授权框;注入 gamescope 自带热键这条路则是死的 —— 在 **nested(Wayland 后端)** 下
`CWaylandInputThread::HandleKey` 拿 **Wayland keycode** 去比 evdev `KEY_*`,差 8,
**永远不命中**,而且只有游戏窗口聚焦时它才收得到键。加上 `GlobalShortcuts` 要求调用方有
app id、KDE 又**不解析** `preferred_trigger`("授权成功" ≠ "按了有用"),这条路被整体删掉。

**剩下的入口只有 CLI**:`kotori scale status|toggle|up|down|reset|fullscreen|fsr|nis|
integer|linear|sharpness` → `scale.action` / `scale.adjust_sharpness` / `scale.toggle_*`。
窗口增减交给 KWin 原生(拖动、平铺)。

---

## 4. 缩放档与比例

全部在 `src/scale/action.rs`,纯函数、有单测。

| 概念 | 值 / 公式 |
|------|-----------|
| `SCALE_LADDER` | `[1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0]` |
| `ladder_index_for(ratio)` | 梯子上离 `ratio` 最近的一格 |
| `ladder_step(index, up)` | 上一格 / 下一格,两端夹住(不环绕) |
| `profile_ratio(profile, screen)` | `output_size_for(screen).width / internal_size().width` |
| `toggle_target(profile, screen)` | 就是 `profile_ratio`,即"设定比例" |
| `toggled_ratio(current, target)` | 已经在 target 上(差 `< RATIO_EPSILON` = 0.001)→ 回 `1.0`,否则去 `target` |
| `session.runtime_ratio` | 会话自己的记录;启动时从档案算,每次窗口动作后更新 |

**比例是记的,不是量的**。窗口几何属于合成器,每次按键都去问它就得给 kotori 配一个
D-Bus 服务;而"只用来决定下一步往哪走"对精度要求不高。所以 `apply_window` 用
`internal_size() × ratio` 反算目标物理像素,再交给 KWin 夹在屏内(夹取由脚本做,所以
"比例太大"的结果是铺满屏幕,而不是把窗口推出屏幕外)。

`ToggleScale` 是**双态**的:目标是"档案里那个比例",再按一次回 1:1;它认"当前是不是已经在
目标上"来决定方向,所以启动时就已经在目标比例上的一局,第一次按是**取消**而不是没反应。
`ScaleUp` / `ScaleDown` / `ResetScale` 是给 CLI 用的阶梯版本。

动作的实际执行在 `GamescopeScaleEngine::apply_action`:**对所有存活会话生效**,watch-only
的会话跳过(它们没有我们的 gamescope)。结果按会话分成 `applied` 与 `failed` 两半上报
—— 部分成功必须两半都说,否则用户会以为没生效再按一次。

---

## 5. 退出看门狗:三条路径

**判据是"游戏没了",不是"gamescope 退了"。** 三条路径最终都要对本次会话那个 prefix 执行
`wineserver -k`,否则关机时 systemd scope 要等满 90s。

### 路径 ①:点叉号 —— gamescope 卡在自己的收尾里

点叉号(或 Alt+F4)后 gamescope 会 `KillAllChildren(SIGTERM)` → `WaitForAllChildren()`,后者是
无上限的 `waitpid(-1)` 循环;wine 的 `winedevice.exe` 不理会 SIGTERM,于是主线程永远停在
`wait4()`,窗口还在但不处理事件。

**判据**:读 `/proc/<gamescope_pid>/wchan`,命中 `do_wait` / `kernel_wait4` / `wait4`
(`teardown::WAITING_FOR_CHILDREN`)才算"卡在收尾";健康状态是 `poll_schedule_timeout` /
`do_epoll_wait`,而 `steamcompmgr` 和 reaper 的 `waitpid` 在别的线程 —— 所以这个判据没有假阳性。

**动作**(`gamescope.rs` 里的 watchdog task):每 `TEARDOWN_POLL`(250ms)看一次 → 命中后
**再等 `TEARDOWN_GRACE`(1.2s)并复查一次**(只看一眼不算卡死)→ 仍卡着就
`kill_session_now(pgid)`:对整个进程组**加子进程树**直接 SIGKILL(跳过 SIGTERM),
紧接着 `close_wine(prefix)`。

### 路径 ②:游戏内部退出 / 启动器交接 —— gamescope 根本没察觉

这时 wine 的游戏进程没了,但 gamescope 主线程仍在 Wayland 事件循环里健康地跑
⇒ 路径 ① 的 `do_wait` 判据**永远不命中**,会话会永久挂死(游戏都关了,`status` 里还挂着)。

**对策**是一个独立的探针 task:目标进程名取 `process_name`,否则取 exe 的**文件名**
(wine 会重写 `argv[0]`);先等它出现(`game_shows_up`,300s 上限;没出现就放弃盯梢,
gamescope 自己退出时仍走正常收尾);每 `GAME_POLL`(500ms)查一次,消失后**保持消失**
`GAME_GONE_GRACE`(1.5s)才算数(wine 分阶段起进程,一次"没在跑"说明不了问题);再看一眼
进程树 —— `process::live_game_processes(root)` 还留着非管道进程说明是启动器交接,继续等;
最后才 `terminate_session(root)` + `close_wine`。

### 路径 ③:关机 / 注销 —— daemon 收到 SIGTERM/SIGINT

daemon 收到信号后 `close_all_sessions()` → 每个会话 `stop_session`(见 §6)。
`daemon.shutdown`(**UI 退出**那条)不走这里,它不碰正在玩的游戏。

### watcher task 与 `Ended` 事件

gamescope 的 `Child` 被一个 watcher task 持有:`child.wait()` → 记录退出码与信号(`code()`
为 `None` 就是被信号杀掉的 —— 真机上 gamescope 常以 SIGABRT 收场,单独看 `code()` 看不出来)
→ `process::wait_until_gone(process_name)`(启动器型游戏里 gamescope 可能先退,真正的游戏还在
跑)→ `close_wine(prefix)` → 从 `sessions` 表移除并广播 `SessionKind::Ended`。
**`Ended` 是退出后自动上传的唯一触发点**,所以"会话永远不结束"不只意味着残留进程,还意味着
存档永远不上传。

### 为什么必须 `wineserver -k`

`winedevice.exe` 既住在**独立的进程组**里(`pgid == 它自己的 pid`),又无视 SIGTERM
⇒ `kill(-pgid)` 与"按子进程树杀"都留得下它。留一个,它所在的 systemd scope 就要等满
`TimeoutStopSec`(90s)。`wine::close_prefix` 用 `wineserver -k` 收掉**恰好那一个 prefix**
(绝不 `pkill wineserver`:一台机器上有多个 prefix)。顺序是
**断进程组/进程树 → `wineserver -k`**,反过来会把还在跑的游戏的服务端杀掉。

时间预算(`scale/teardown.rs`,另有 `wine::WINESERVER_KILL_TIMEOUT` = 2s):
`TEARDOWN_POLL` 250ms、`TEARDOWN_GRACE` 1.2s、`GAME_POLL` 500ms、`GAME_GONE_GRACE` 1.5s、
`GROUP_GRACE_STEPS × GROUP_POLL` = 30 × 100ms = 3s。

---

### `stop_session` 与三条路径的关系

`ScaleEngine::stop_session`(`gamescope.rs`):会话不在表里 → `SessionNotFound`(客户端据此
把"点停止没反应"说清楚);`process_group` 是 `None`(watch-only)→ 丢掉会话就是停止,没有
进程要杀;否则 `terminate_session(pgid)` —— **先快照进程树**(root 一死子进程会被 reparent,
再走就找不到了)→ 对进程组与树里每个 pid 发 SIGTERM → 最多等 3s → 再补 SIGKILL;最后
`close_wine(session.wine_prefix)`,由 watcher 负责移除会话 + 发 `Ended`。

路径 ① 只做其中 SIGKILL 那一下(进程组**加子进程树**,跟 `terminate_session` 相比只是跳过
先 SIGTERM 再等 3s 那一步),依赖紧随其后的 `close_wine` 兜住 `winedevice.exe`;路径 ②③走
完整的 `terminate_session`。

---

## 6. 已知未做

- **窗口落在哪块屏没被复查**:`-W/-H` 用的是**主屏**分辨率,窗口若被 KWin 放到另一块屏,
  没有"按它实际所在那块屏再修一次"。`desktop::kde` 本来就具备读那块屏几何的能力,但目前
  **只在运行时动作里被调用**,启动路径没有调它(`grep resize_window` 只有一处调用)。
- **游戏真实渲染尺寸从未被探测**:`internal_*` 是用户手填的,4:3 的 galgame 在 16:9 的
  虚拟屏里居中渲染,黑边会被 FSR 一起放大 —— 这才是"两边空置"的根因。
- **daemon 被 SIGKILL 时正在跑的会话当场无人收尾**:信号路径只在 SIGTERM/SIGINT 下有效,
  SIGKILL 当下 `close_all_sessions` 跑不到。但每局启动时会把 prefix 记进数据目录
  (`wine_prefixes.rs`),下一次 daemon 收到关机信号时顺手把这些没人认领的 prefix 用
  `wineserver -k` 关掉,所以 `winedevice.exe` 不会无限期赖着。要根治仍得让每局游戏
  自己进一个 cgroup,收尾写 `cgroup.kill`。**收尾时 gamescope 常以 SIGABRT 结束**,KDE
  于是弹崩溃通知(用户已决定暂不处理)。

`TODO(未核对)`:gamescope 默认内部分辨率恰为 1280×720、`-s` 在 3.16 已改为
`--mouse-sensitivity`、`winedevice.exe` 无视 SIGTERM、nested 窗口尺寸直接写进 `g_nOutput`、
keycode 偏移 8 —— 都只来自代码注释与单测的契约,没有读 gamescope/wine 源码或真机复现。
三条退出路径中,**叉号那条**与**"跑一局之后关机不再等 90 秒"**同样没有真机复核。

**Windows 上没有缩放可做**(`src/scale/unsupported.rs`):`start_session` 等真正"做缩放"的
动作一律回 `ScaleError::Unsupported`(`stop_session` 刻意回 `Ok`:没有会话可停,再报错只是
噪音),`scale_rpc` 的 `run_action` 对动作统一回"这个平台做不到"(`src/daemon/scale_rpc.rs`,
刻意不给空后端补同名方法去凑合)。能力表据此把缩放相关的编辑设为**不适用**(不是"缺失"),
界面显示成"缩放归外部工具"而不是报错。缩放本身归 Magpie,而 Magpie 只让观察、不让下命令,
所以这篇里"启动 gamescope""运行时改缩放""退出收尾"三套东西到 Windows 上全不成立;连
Magpie 观察后端都**还没写**(PLATFORMS.md §2.3 / §2.7)—— 别把"没接"当成"已经能看了"。
