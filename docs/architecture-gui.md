# GUI(Slint)

**这篇讲什么**:为什么前端是 Slint、`src/ui/` 的四层各管什么、Elm 风格的
`update(Message) -> Task` 怎么跑、单游戏页"改一下就自动保存"的三条规矩,以及**没有显示器
时怎么验证界面**。**什么时候读它**:动界面、动 `src/ui/` 任何一层,或者要给 UI 加一个
新的用户操作的时候。

---

## 1. 为什么是 Slint

目标形态是 WinUI 3(左侧 NavigationView + 右侧卡片组 + Fluent 控件 + 动效),
2026-09-13 从 iced 换成 Slint,理由只有两条是硬的:

- **iced 没有动画系统、没有焦点环、控件内部样式不可控**。静态能凑到 85~90%,但 Win11 的
  辨识度一半在"动"。
- **iced 在 Wayland 下接不上输入法**(fcitx5 打不出中文);Slint 下实测正常。

Rust 内的取舍上,Slint 有内建 `animate`(`duration`/`delay`/`easing`)、自带 `fluent` 风格、
Windows 上默认就是 fluent、可静态链接成单 exe、Linux ARM64 是它的主场。代价是 GPLv3 或
"带署名"的免费桌面许可。

`build.rs` 用 `slint-build` 编译 `src/ui/slint/app.slint`,风格 `fluent`,并开启
`with_debug_info`(给 §6 的几何断言用)。`Cargo.toml` 里 `slint = "1.17"` 带
`compat-1-2`;`png` 只给快照用;`i-slint-backend-testing` 是 **dev-dependency**,不进二进制。

**标题栏用系统的**(`app.slint` 里 `no-frame: false`)。自绘标题栏(KWin 下)会把窗口卡进
"永不结束的交互式移动",根因是 winit 的 `latest_button_serial()` 按下与抬起都会覆盖,
而 `xdg_toplevel.move` 要的是按下那一刻的 serial —— Slint 隔了一层回调拿不到。

### 入口:不给子命令就是 UI

```rust
let command = cli.command.unwrap_or(cli::Command::Ui);   // src/main.rs
```

双击 `kotori.exe`(Windows)、点桌面图标(Linux)、在终端里直接敲 `kotori`,走的是同一条路。
这不是图省事:GUI 应用双击之后先弹一段 help、还要用户自己猜该敲哪个子命令,是没道理的。
要看帮助仍然有 `kotori --help`(它照旧列出全部子命令)。

### 图形后端:起不来就换软件渲染

Slint 的渲染器是**编译期**定的(`renderer-femtovg` 这个 feature),运行时只有"要求用
某一个"这一种表达方式,而且 `i_slint_core::platform::set_platform` **只允许成功一次** ——
所以"先试 OpenGL,不行再换软件渲染"没法在同一个进程里做第二遍。

没有 GPU 的机器(虚拟机、远程桌面、只有「Microsoft 基本显示适配器」的系统)上,femtovg
会在建窗口那一刻失败(`src/ui/driver.rs` 的 `AppWindow::new()`):

```text
Error: Failed to initialize OpenGL driver: Could not locate glCreateShader symbol
```

Windows 自带的 `opengl32.dll` 只到 OpenGL 1.1,而 femtovg 要 2.0+ —— `glCreateShader`
正是 2.0 才有的符号。**Slint 自己不会回退**:`i-slint-backend-winit` 的 `create_renderer`
在没指定渲染器时直接走编译期的默认值,那个 `allow_fallback` 只管"名字不认识",不管
"初始化失败"。

`src/ui/backend.rs` 负责收尾:捕获这个失败,带着 `SLINT_BACKEND=winit-software`
**把自己重启一次**(用重启而不是重试,就是因为上面那条 `set_platform` 只能成功一次),
并留一个环境变量记住"已经退过一次",免得软件渲染也起不来时无限重启。**用户显式设过
`SLINT_BACKEND` 就不覆盖** —— 他既然写明了,报错比背着他换掉诚实。

---

## 2. 分层:哪些是新的,哪些只是拆了目录

> 搬 Slint 时明确"不改语义"的五层(`message`/`app`/`tasks`/`model`/`parse`)如今只剩
> `message`/`app`/`tasks` 还是单文件;`model.rs`、`parse.rs`、`update.rs` 已按域拆成
> `model/`、`parse/`、`update/` 三个目录(**语义一字未动,物理拆分**,见各目录头注释)。
> 「一行没动」这句话本身已经过时,要说清楚的是:拆的是文件,不是行为。

```
            .slint  (声明式视图, 页面自己持有输入框内容与下拉状态)
              ▲ 属性(只推)                     │ 回调
              │                                ▼
   render/  状态 → 属性               wire.rs  回调 → Message
              ▲                                │
              │                                ▼
              └──────── app.rs / update/  (App 状态 + update(Message) -> Task<Message>)
                                 │
                                 ▼
                        tasks.rs  (每个 Task 就是一条 async 的 RPC)
                                 │
                                 ▼
                       driver.rs (消息循环 + tokio + 线程本地状态)
```

| 层 | 文件 | 职责 | 规矩 |
|----|------|------|------|
| 视图 | `src/ui/slint/`(`app.slint` + `app-nav.slint` + `widgets/` + `pages/`) | 摆放与接线;**窗口自己的状态**(页签、输入框内容)由它持有 | 不写 `width`/`height`,不自己定位浮层 |
| 状态→属性 | `src/ui/render/`(一页一个文件 + `mod.rs` + `window_test/`) | 把 `App` 的字段推成窗口属性 | **只做映射,不含判断**;所有 push 都"先比再写" |
| 回调→消息 | `src/ui/wire.rs` | 每个 `on_*` 回调构造一条 `Message` | **这里不做任何决定**;规则若出现在这个文件里,就该挪去 `update/` |
| 消息循环 | `src/ui/driver.rs` | `dispatch(Message)` = `update` → `render` → 启动 Task;结果用 `invoke_from_event_loop` 回到 UI 线程 | 只有 `Message` 跨线程 |
| 副作用 | `src/ui/tasks.rs` | 每条 Task 就是一次 `rpc::call` | 结果一律包成 `Message` 回来 |
| 状态 | `src/ui/app.rs` | `App` 的全部字段 + 不属消息循环的 `impl` | |
| 消息 | `src/ui/message.rs` | `Message` / `Tab` / `SyncField` / `PathTarget` | |
| 数据 | `src/ui/model/` | 普通数据类型 + 常量(`STATUS_POLL` 3s、`AUTOSAVE_DEBOUNCE` 700ms),按域拆成 `{sync,game,session,environment}.rs` | |
| 解析 | `src/ui/parse/` | `serde_json::Value` ↔ 结构体,按域拆成 `{sync,games,save_paths,scale,wine,environment,reconnect}.rs` | |
| 崩溃日志 | `src/ui/crash.rs` | panic hook 写 `<data_dir>/logs/ui-crash.log` | |
| 字体 | `src/ui/font.rs` | 挑一个系统里**确实有**的字体 | 绝不打包微软字体 |
| 快照 | `src/ui/snapshot.rs` | 调试截图(仅 debug 构建) | 见 §6 |

`update/`、`parse/`、`model/` 是把原来三个大文件(`update.rs`、`parse.rs`、`model.rs`)
按域拆成的目录;
`task.rs` 取代的是 `iced::Task`(只有 `Task::none` / `Task::perform` / `Task::batch` 与
`into_effects`)。

`Ui`(`driver.rs`)额外持有三样窗口存不下的东西:`VecModel<GameItem>`、`VecModel<SaveItem>`
(**Slint 的数组属性不可变,没有 `push`、不能按下标赋值**),以及两个种子号
`detail_seed` / `saves_seed`。

---

## 3. Elm 风格的消息循环

```rust
// driver.rs
pub(super) fn dispatch(message: Message) {
    let task = with_ui(|ui| { let t = ui.app.update(message); render(ui); t });
    spawn(task);              // 每条 effect 一个 tokio task
}
```

- `App::update(&mut self, Message) -> Task<Message>` 是**同步纯逻辑**:改状态、返回副作用。
- `spawn` 把每条 effect `runtime.spawn` 到 `main` 已经建好的 tokio runtime 上;完成时
  `slint::invoke_from_event_loop(|| dispatch(msg))` 把结果送回 UI 线程。窗口已经关掉时
  这里会失败,只记一条 debug —— "关窗后到达的回包"没有别的意义。
- 状态放在 **thread_local**(`UI: RefCell<Option<Rc<RefCell<Ui>>>>`)而不是被闭包捕获:
  完成闭包必须 `Send`(它要跨回事件循环),而窗口不是 `Send`;只有 `Message` 过界,闭包在
  另一侧重新把状态查出来。
- **窗口属性与 `App` 字段是两份状态**。页签就是例子:`.slint` 点导航栏时自己改 `tab` 属性
  并补发 `tab-changed`,Rust 侧的 `App::tab` 落一拍。写测试时要**两边都设**
  (`window_test/mod.rs::show_tab`),否则页面不会实例化,断言会全部空跑。

---

## 4. 单游戏页的自动保存:三条规矩

单游戏页**没有「保存」按钮**,改一下就防抖写回,页脚一行小字汇报
`保存中…` / `已自动保存` / `保存失败: …`。三条规矩缺一不可
(`app.rs` + `update/`,都有单测):

### ① 世代号防抖

`schedule_auto_save()`:每一笔编辑 `autosave_generation += 1`,并挂一个
`AUTOSAVE_DEBOUNCE`(700ms)的定时器,醒来发 `Message::AutoSave(generation)`。
`AutoSave(g)` 里 `g != self.autosave_generation` 就**直接作废** —— 说明这 700ms 里用户又
改了。所以连打一串字只写一次,而且写的永远是最新那份草稿。

`cancel_auto_save()` 也是把世代 +1(换游戏、返回列表、按「重置」时用)。

### ② 同一时刻只允许一笔写

`begin_auto_save()` 见到 `save_in_flight.is_some()` 就**退回**(只记一条 debug)。
理由是硬的:daemon 并发处理请求,而 `game.update` 收的是**整份 profile**,两次全量写重叠
时后到的旧快照会盖掉新的 —— 不是理论风险。

不用担心"丢了一笔":在路上的那一笔回来时世代必然已经变了,规矩 ③ 会补上。

### ③ 过期回包必须补存一份

`ProfileSaved(generation, result)`:

1. 取出 `save_in_flight`(`saving = false`)。
2. **回包只能落在它自己那份草稿上**:`same_game = selected == attempt.draft.game_id`。
   用户可能已经翻到别的游戏,拿这份回包去改别人的 `*_original` 会把书签写脏。
3. 成功时把 `game_dir_original` / `exe_original` / `save_paths_original` 推进到
   **这一笔带过去的值**(不是手上这份);少了这一下,下次比较会一直认为"路径变了",
   游戏盘没挂载时"改个锐度也存不进去"(`save_profile` 只在路径变了时才发路径)。
4. 失败时**草稿一个字都不动**(用户正在输入的内容不能被回包吃掉);已经离开那一页了就把
   失败挂到顶部错误条。
5. `same_game && generation != autosave_generation` ⇒ **`begin_auto_save()` 再存一次**
   (服务端现在写着一个用户不要的值)。
6. 然后重读一遍游戏库(`game.list`),列表里的"已存值"才跟得上;页面自己的副本不会被它
   重置(`detail_seed` 没动)。

另外两条与自动保存配套的机制:

- **`detail_seed`**:单游戏页持有可编辑副本,靠"种子变了"来重新按已存值抄一份。进入游戏
  与按「重置」时 `Ui::reseed_detail()` 会 +1。`wire.rs` 里 `on_open_game` / `on_reset` 必须
  在消息派发**之后**再 bump,否则抄到的是旧值。
- **「浏览…」的 `PathPick` 令牌**(`types.slint`):输入框内容归页面所有,Rust 平时不往里
  写;**值一样也要能触发一次**,所以推的是一个递增令牌而不是值本身。⚠ 这一条在
  `window_test/mod.rs` 里**故意没有断言** —— 测试后端的 `accessible_value()` 读到的是旧值,
  写一条会撒谎的断言比不写更糟;只能靠快照看(§6)。

---

## 5. 视图层的硬约束(踩过的,别重犯)

`render/` 的两条总规矩:

1. **绝不写回一个已经在那里的值**。Slint 的 `in-out` 属性是输入控件自己的,从外面原样写回
   会把光标弹到行尾。所有 push 都先比较。
2. **整表只在形状变了时重建**。`set_vec` 会重建每一行、正在输入的那一行会丢焦点。存档列表
   (`render/detail.rs::push_saves`)因此在"进入游戏"或"增删一行"时才 `set_vec`,单独改
   kind 用 `set_row_data`,纯文本编辑**完全不回推**。

`.slint` 侧最要命的几条(细节以 `UI_GUIDE.md` §7 为准):

| 约束 | 后果 |
|------|------|
| `Window` 上别写 `width`/`height` | Slint 会算成 `min == max` 并 `set_resizable(false)`,连平铺都失效 |
| `min-width` 必须**小于** `preferred-width` | 窗口一窄整页横向溢出(卡片被切、文字竖排) |
| 布局里 `visible: false` **照样占位** | 要真藏掉一行得用 `if cond : X { … }` |
| 浮层必须画在 `ScrollView` **之外** | 否则被视口裁掉;它不能自己定位、不能逐帧动画 |
| Slint 没有位移原语 | 入场动画只能动 `padding-*` |
| 同一组件里属性与回调不能重名 | 回调加 `-clicked` 后缀 |
| `enabled` 被 `ScrollView` 占了 | 我们的按钮属性叫 `button-enabled` |
| **一行可点 + 里面有按钮:整行的 `TouchArea` 必须声明在最前面** | 否则会把右边的按钮整个盖住(点「启动」没反应,反而进详细设置) |
| `TextInput` 默认顶对齐 | 要显式 `vertical-alignment: center` |

字体:`font.rs` 按 `Microsoft YaHei UI` → `Segoe UI Variable` → `Noto Sans CJK SC` →
`Sarasa Gothic SC` 顺序找,用 `fc-match` 检查"它返回的第一个族名是不是我要的"(缺字体时
fontconfig 会回**替代品**,不比对就等于永远成功)。都不在就不动窗口默认值,交给 Slint 的
逐字形回退。**绝不打包微软字体**。

---

## 6. 没有显示器时怎么验证 UI

三件工具,前两件是 `cargo test` 的一部分,第三件要手动跑。

### (a) 整页渲染测试 —— `src/ui/render/window_test/`

用 `i_slint_backend_testing::init_no_event_loop()` 在**没有显示器**的情况下建出真窗口,
然后逐页、逐状态 `render()` 一遍:游戏库(空/有/搜索命中/搜索不命中/运行中/仅观测/启动中)、
单游戏页(含三种存档位置 kind)、添加页(含**云端匹配块**的各状态:正在问/唯一命中/多条
候选/云端没有/没问成,以及「自己选…」那个带搜索的浮层)、**「云端存档」页**(未读/列表/
详情/搜索/深度扫描)、云同步页(未读到/就绪/待确认恢复/凭据文件锁着与
解锁后/删除凭据文件前的二次确认条/只有内存/没装 rclone 且有解析不了的存档位置/**kopia
模式的折叠与展开**/**kopia 没装时那一组变红**)、设置页(含「环境检查」的三种状态各一行:
可用 / 有条件 / 缺少,以及结果还没到时的空表)。

它顶替的是"翻到那一页才炸"的那类运行时错误:`Select.options[selected]` 越界、`for` 到空
数组、下标→枚举映射写反、属性忘填。

**同一支测试还量几何**:断言 `SyncCredentialsGroup` 与 `CardRow` 这类最宽的行
**不比窗口宽**(`min-width` 撑破窗口这种错肉眼要量像素才发现)。两个前提:

- `build.rs` 必须开 `with_debug_info`,否则 `ElementHandle` 静默返回空列表,断言等于没写
  (查不到就直接 panic,不放过);
- `ElementHandle` 只看得见**没被裁掉**的部分,所以量之前先把窗口撑到 1120×2600。

⚠ 这里**测不了**"「浏览…」挑回来的值有没有真的进输入框"(见 §4 的 `PathPick`)。

### (b) 命中测试 —— `src/ui/render/hit_test.rs`

几何断言查不出"有东西盖在上面"。这支测试发**真的指针事件**
(`WindowEvent::PointerMoved/Pressed/Released`,坐标是**逻辑像素**),量两件事:

1. 点那一行的「启动」按钮 ⇒ 真的去启动,**且没有**顺带进详细设置;
2. 点行里的空白 ⇒ 进详细设置(整行可点这条不能被修没了)。

⚠ 点击可能让宿主**整表重建**游戏库,旧 `ElementHandle` 会失效(几何回 0),所以坐标要
**先量成普通数字**再点。

### (c) 快照与调试变量 —— `src/ui/snapshot.rs`(仅 debug 构建)

```
KOTORI_UI_SNAPSHOT=/tmp/ui.ppm      # 拍一张 PPM 到该路径然后退出
KOTORI_UI_SNAPSHOT_DELAY=2500       # 什么时候拍(默认 2500ms:第一次加载要问 daemon)
KOTORI_UI_SEED_DELAY=1500           # 什么时候套用下面这几个状态
KOTORI_UI_TAB=0..4                  # 0 游戏库 / 1 添加 / 2 云端存档 / 3 云同步 / 4 设置
KOTORI_UI_SELECT=<game id>          # 并打开单游戏页
KOTORI_UI_SEARCH=<text>
KOTORI_UI_PICK=<path>               # 假装文件对话框返回了这个路径(唯一能验证 §4 令牌链的办法)
```

PPM 是刻意的:不需要编码器,一行 Python 就能看。状态是通过**消息循环**套上去的
(`KOTORI_UI_TAB` 例外:页签是窗口自己的属性,所以属性与消息都要设),拍的因此是那个状态
真实的页面,而不是被硬塞过属性的壳。

---

## 7. 已知未做

- **`update`/`parse`/`model` 已按域拆成目录**(`update/` 7 个文件、`parse/` 10 个、
  `model/` 9 个),`app.rs` 也拆到了 477 行(测试挪去了 `app/tests_*.rs`);
  `update/` 里没有一个文件超过 500 行,总算不再有"巨型 match"。
  ⚠ 这几个数只是"当时的样子",别当契约 —— 拆分还会继续。
- **云端版本看得到,但删不下**:「云端存档」页能看到某一款每一版(版本名、大小、时间,
  走 `sync.cloud_versions`);但删除某一版、下载 / 导出某一版**没有界面**(后端
  `sync.cloud_versions` / `sync.restore` 在)。单游戏页也仍然只有「恢复最新」,没有
  版本选择器。
- **前端细节要重测**:中文输入法打完字、动画手感、缩放/平铺行为,都等用户实测。
- **毛玻璃最后做**,而且**只做自家半透明质感**,不依赖合成器协议。

`TODO(未核对)`:测试条数(单测 + E2E 的总数)、`cargo fmt` / `clippy` 是否零告警、
以及"某条具体测试是否仍然通过"—— 本次没有运行 `cargo test`(沙箱里需要
`CARGO_HOME=.cargo-home` 与 `--offline`),只读了测试代码。
