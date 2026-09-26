# GUI(Slint)

**这篇讲什么**:为什么前端是 Slint、`src/ui/` 的分层各管什么、Elm 风格的 `update(Message) -> Task` 怎么跑、单游戏页「改一下就自动保存」的三条规矩、视图层的硬约束、以及**没有显示器时怎么验证界面**。

**什么时候读它**:动界面；动 `src/ui/` 任何一层；要给 UI 加一个新的用户操作。

核对来源:`src/ui/**`、`src/ui/slint/**`、`build.rs`、`Cargo.toml`。

---

## 1. 为什么是 Slint

目标形态是 WinUI 3(左侧导航 + 右侧卡片组 + Fluent 控件 + 动效)。从 iced 换成 Slint,理由只有两条是硬的:

- **iced 没有动画系统、没有焦点环、控件内部样式不可控**。静态能凑到八成,但 Win11 的辨识度一半在「动」。
- **iced 在 Wayland 下接不上输入法**(fcitx5 打不出中文);Slint 下实测正常。

Rust 内的取舍上,Slint 有内建 `animate`(`duration` / `delay` / `easing`)、自带 `fluent` 风格、Windows 上默认就是 fluent、可静态链接成单 exe、Linux ARM64 是它的主场。代价是 GPLv3 或「带署名」的免费桌面许可。

`build.rs` 用 `slint-build` 编译 `src/ui/slint/app.slint`,风格 `fluent`,并开启 `with_debug_info`(给 §6 的几何断言用)。`i-slint-backend-testing` 是 **dev-dependency**,不进二进制。

**标题栏用系统的**(`app.slint` 里 `no-frame: false`)。自绘标题栏在 KWin 下会把窗口卡进「永不结束的交互式移动」,根因是 winit 的按钮 serial 按下与抬起都会覆盖,而 `xdg_toplevel.move` 要的是按下那一刻的 serial —— Slint 隔了一层回调拿不到。

### 入口:不给子命令就是 UI

```rust
let command = cli.command.unwrap_or(cli::Command::Ui);   // src/main.rs
```

双击 `kotori.exe`(Windows)、点桌面图标(Linux)、在终端里直接敲 `kotori`,走的是同一条路。这不是图省事:GUI 应用双击之后先弹一段 help、还要用户自己猜该敲哪个子命令,是没道理的。要看帮助仍然有 `kotori --help`。

### 图形后端:起不来就换软件渲染

Slint 的渲染器是**编译期**定的,运行时只有「要求用某一个」这一种表达方式,而且 `set_platform` **只允许成功一次** —— 所以「先试 OpenGL,不行再换软件渲染」没法在同一个进程里做第二遍。

没有 GPU 的机器(虚拟机、远程桌面、只有「Microsoft 基本显示适配器」的系统)上,建窗口那一刻会失败:

```text
Error: Failed to initialize OpenGL driver: Could not locate glCreateShader symbol
```

Windows 自带的 `opengl32.dll` 只到 OpenGL 1.1,而渲染器要 2.0+ —— `glCreateShader` 正是 2.0 才有的符号。**Slint 自己不会回退**:没指定渲染器时它直接走编译期的默认值,那个 `allow_fallback` 只管「名字不认识」,不管「初始化失败」。

`src/ui/backend.rs` 负责收尾:捕获这个失败,带着 `SLINT_BACKEND=winit-software` **把自己重启一次**(用重启而不是重试,就是因为上面那条只能成功一次),并用 `KOTORI_RENDERER_RETRIED` 记住「已经退过一次」,免得软件渲染也起不来时无限重启。**用户显式设过 `SLINT_BACKEND` 就不覆盖** —— 他既然写明了,报错比背着他换掉诚实。

---

## 2. 分层

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
                        task.rs  (每个 Task 就是一条 async 的 RPC)
                                 │
                                 ▼
                       driver.rs (消息循环 + tokio + 线程本地状态)
```

| 层 | 位置 | 职责 | 规矩 |
|----|------|------|------|
| 视图 | `src/ui/slint/`(`app.slint` + `app-nav.slint` + `state/` + `widgets/` + `pages/` + `theme.slint` + `types.slint`) | 摆放与接线;**窗口自己的状态**(页签、输入框内容)由它持有 | 不写 `width`/`height`,不自己定位浮层 |
| 状态→属性 | `src/ui/render/`(一页一个文件 + `window_test/` + `hit_test.rs`) | 把 `App` 的字段推成窗口属性 | **只做映射,不含判断**;所有 push 都「先比再写」 |
| 回调→消息 | `src/ui/wire.rs` | 每个 `on_*` 回调构造一条 `Message` | **这里不做任何决定**;规则若出现在这个文件里,就该挪去 `update/` |
| 消息循环 | `src/ui/driver.rs` | `dispatch(Message)` = `update` → `render` → 启动 Task;结果用 `invoke_from_event_loop` 回到 UI 线程 | 只有 `Message` 跨线程 |
| 副作用 | `src/ui/task.rs` | 每条 Task 就是一次 `rpc::call` | 结果一律包成 `Message` 回来 |
| 状态 | `src/ui/app.rs` + `app/` | `App` 的全部字段 + 不属消息循环的 `impl` | |
| 消息 | `src/ui/message.rs` | `Message` / `Tab` / `SyncField` / `PathTarget` | |
| 数据 | `src/ui/model/` | 普通数据类型 + 常量(状态轮询 3s、自动保存防抖 700ms) | |
| 解析 | `src/ui/parse/` | `serde_json::Value` ↔ 结构体 | |
| 崩溃日志 | `src/ui/crash.rs` | panic hook 写 `<data_dir>/logs/ui-crash.log` | |
| 字体 | `src/ui/font.rs` | 挑一个系统里**确实有**的字体 | 绝不打包微软字体 |
| 快照 | `src/ui/snapshot.rs` | 调试截图(仅 debug 构建) | 见 §6 |

`update/`、`parse/`、`model/` 是三个按域拆开的目录。拆的是文件,不是行为 —— 语义从 iced 时代一路没动过。

跨页的表单状态一律走 Slint 全局,根窗口只做摆放与转发。新增一族字段照这个来,别再往根窗口上堆。

`Ui`(`driver.rs`)额外持有三样窗口存不下的东西:游戏列表与存档列表两个数组属性(**Slint 的数组属性不可变,没有 `push`、不能按下标赋值**),以及两个种子号。

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
- `spawn` 把每条 effect 派到 `main` 已经建好的 tokio runtime 上;完成时用 `slint::invoke_from_event_loop` 把结果送回 UI 线程。窗口已经关掉时这里会失败,只记一条 debug —— 「关窗后到达的回包」没有别的意义。
- 状态放在 **thread_local** 而不是被闭包捕获:完成闭包必须 `Send`(它要跨回事件循环),而窗口不是 `Send`;只有 `Message` 过界,闭包在另一侧重新把状态查出来。
- **窗口属性与 `App` 字段是两份状态**。页签就是例子:`.slint` 点导航栏时自己改 `tab` 属性并补发 `tab-changed`,Rust 侧的 `App::tab` 落一拍。写测试时要**两边都设**,否则页面不会实例化,断言会全部空跑。

导航共五项:`Game` / `AddGame` / `Cloud`(云端存档) / `Sync`(云同步) / `Settings`。

---

## 4. 单游戏页的自动保存:三条规矩

单游戏页**没有「保存」按钮**,改一下就防抖写回,页脚一行小字汇报。三条规矩缺一不可:

### ① 世代号防抖

每一笔编辑把世代号 +1,并挂一个防抖定时器,醒来发一条带世代号的自动保存消息。醒来时世代号对不上就**直接作废** —— 说明这 700ms 里用户又改了。所以连打一串字只写一次,而且写的永远是最新那份草稿。换游戏、返回列表、按「重置」时同样把世代号 +1 作废掉在途的定时器。

### ② 同一时刻只允许一笔写

见到「已有一笔在飞」就**退回**(只记一条 debug)。理由是硬的:daemon 并发处理请求,而 `game.update` 收的是**整份 profile**,两次全量写重叠时后到的旧快照会盖掉新的 —— 不是理论风险。

不用担心「丢了一笔」:在路上的那一笔回来时世代必然已经变了,规矩 ③ 会补上。

### ③ 过期回包必须补存一份

一笔保存回来时:

1. 取出在途的那笔,清掉「在飞」标记。
2. **回包只能落在它自己那份草稿上**。用户可能已经翻到别的游戏,拿这份回包去改别人的「已存值」会把书签写脏。
3. 成功时把「已存值」推进到**这一笔带过去的值**(不是手上这份)。少了这一下,下次比较会一直认为「路径变了」,游戏盘没挂载时「改个锐度也存不进去」。
4. 失败时**草稿一个字都不动**(用户正在输入的内容不能被回包吃掉);已经离开那一页了就把失败挂到顶部错误条。
5. 同一款游戏且世代已经变了 ⇒ **再存一次**(服务端现在写着一个用户不要的值)。
6. 然后重读一遍游戏库,列表里的「已存值」才跟得上;页面自己的副本不会被它重置(种子号没动)。

另外两条与自动保存配套的机制:

- **种子号**:单游戏页持有可编辑副本,靠「种子变了」来重新按已存值抄一份。进入游戏与按「重置」时会 +1。`wire.rs` 里对应回调必须在消息派发**之后**再 bump,否则抄到的是旧值。
- **「浏览…」的令牌**:输入框内容归页面所有,Rust 平时不往里写;**值一样也要能触发一次**,所以推的是一个递增令牌而不是值本身。⚠ 这一条在整页渲染测试里**故意没有断言** —— 测试后端读到的 `accessible_value` 是旧值,写一条会撒谎的断言比不写更糟;只能靠快照看(§6)。

⚠ 路径组、存档位置组、缩放组**各有自己的保存按钮**,离开页面直接丢弃 —— 改路径和改锐度不该绑成一次写。

---

## 5. 视图层的硬约束

`render/` 的两条总规矩:

1. **绝不写回一个已经在那里的值**。Slint 的 `in-out` 属性是输入控件自己的,从外面原样写回会把光标弹到行尾。所有 push 都先比较。
2. **整表只在形状变了时重建**。重建每一行会让正在输入的那一行丢焦点。存档列表因此在「进入游戏」或「增删一行」时才重建,单独改类型用改单行,纯文本编辑**完全不回推**。

`.slint` 侧最要命的几条:

| 约束 | 后果 |
|------|------|
| `Window` 上别写 `width`/`height` | Slint 会算成 `min == max` 并把窗口设成不可调整大小,连平铺都失效 |
| `min-width` 必须**小于** `preferred-width` | 窗口一窄整页横向溢出(卡片被切、文字竖排) |
| 布局里 `visible: false` **照样占位** | 要真藏掉一行得用 `if cond : X { … }` |
| **浮层必须画在 `ScrollView` 之外** | 否则被视口裁掉;它不能自己定位、不能逐帧动画 |
| **浮层卡片里的布局必须自己声明高度** | 普通 `Rectangle` 里的布局默认填满父节点,而卡片高度按「内容 + 上下各 24px」算,差的那 24px 会被摊到各项之间,最后一行被顶出卡片下沿。写法:内容单独占一块,高度取 `layout.preferred-height` |
| Slint 没有位移原语 | 入场动画只能动 `padding-*` |
| 同一组件里属性与回调不能重名 | 回调加 `-clicked` 后缀 |
| `enabled` 被 `ScrollView` 占了 | 我们的按钮属性叫 `button-enabled` |
| **一行可点 + 里面有按钮:整行的 `TouchArea` 必须声明在最前面** | 否则会把右边的按钮整个盖住(点「启动」没反应,反而进详细设置) |
| `TextInput` 默认顶对齐 | 要显式 `vertical-alignment: center` |

宽度断言查不出高度那一类错误,只能量 —— 浮层卡片的高度问题全是这么发现的。

字体按 `Microsoft YaHei UI` → `Segoe UI Variable` → `Noto Sans CJK SC` → `Sarasa Gothic SC` 顺序找,用 `fc-match` 检查「它返回的第一个族名是不是我要的」(缺字体时 fontconfig 会回**替代品**,不比对就等于永远成功)。都不在就不动窗口默认值,交给 Slint 的逐字形回退。**绝不打包微软字体**。

---

## 6. 没有显示器时怎么验证 UI

三件工具,前两件是 `cargo test` 的一部分,第三件要手动跑。

### (a) 整页渲染测试 —— `src/ui/render/window_test/`

用测试后端在**没有显示器**的情况下建出真窗口,然后逐页、逐状态渲染一遍:游戏库(空/有/搜索命中/搜索不命中/运行中/仅观测/启动中)、单游戏页(含三种存档位置类型)、添加页(含云端匹配块的各种状态)、**「云端存档」页**(未读/列表/详情/搜索/深度扫描)、云同步页(未读到/就绪/待确认恢复/凭据锁着与解锁后/删除前的二次确认条/只有内存/没装引擎/引擎模式折叠与展开)、设置页(含环境检查的三种状态各一行)。

它顶替的是「翻到那一页才炸」的那类运行时错误:下标越界、`for` 到空数组、下标→枚举映射写反、属性忘填。

**同一支测试还量几何**:断言最宽的行**不比窗口宽**(`min-width` 撑破窗口这种错肉眼要量像素才发现)。两个前提:

- `build.rs` 必须开 `with_debug_info`,否则元素句柄静默返回空列表,断言等于没写(查不到就直接 panic,不放过);
- 元素句柄只看得见**没被裁掉**的部分,所以量之前先把窗口撑到足够大。

### (b) 命中测试 —— `src/ui/render/hit_test.rs`

几何断言查不出「有东西盖在上面」。这支测试发**真的指针事件**(坐标是**逻辑像素**),量两件事:

1. 点那一行的「启动」按钮 ⇒ 真的去启动,**且没有**顺带进详细设置;
2. 点行里的空白 ⇒ 进详细设置(整行可点这条不能被修没了)。

⚠ 点击可能让宿主**整表重建**游戏库,旧的元素句柄会失效(几何回 0),所以坐标要**先量成普通数字**再点。

### (c) 快照与调试变量 —— `src/ui/snapshot.rs`(仅 debug 构建)

```
KOTORI_UI_SNAPSHOT=/tmp/ui.ppm      # 拍一张 PPM 到该路径然后退出
KOTORI_UI_SNAPSHOT_DELAY=2500       # 什么时候拍(默认 2500ms:第一次加载要问 daemon)
KOTORI_UI_SEED_DELAY=1500           # 什么时候套用下面这几个状态
KOTORI_UI_TAB=0..4                  # 0 游戏库 / 1 添加 / 2 云端存档 / 3 云同步 / 4 设置
KOTORI_UI_SELECT=<game id>          # 并打开单游戏页
KOTORI_UI_SEARCH=<text>
KOTORI_UI_PICK=<path>               # 假装文件对话框返回了这个路径(唯一能验证令牌链的办法)
```

PPM 是刻意的:不需要编码器,一行 Python 就能看。状态是通过**消息循环**套上去的(`KOTORI_UI_TAB` 例外:页签是窗口自己的属性,所以属性与消息都要设),拍的因此是那个状态真实的页面,而不是被硬塞过属性的壳。

---

## 7. 已知未做

- **云端存档管理只有删除**。「云端存档」页已经能浏览云端所有游戏、每一款的每一版,并且三个入口的删除都接好了(删一版 / 清空一款 / 删掉词条)。但**下载某一版、导出、备注、保留策略都还没有**。单游戏设置那侧有「替换」与「取回」,仍没有版本选择器。
- **前端细节要实测**:中文输入法打完字、动画手感、缩放与平铺行为,都等用户侧确认。
- **毛玻璃最后做**,而且只做自家半透明质感,不依赖合成器协议。
