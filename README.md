<div align="center">

# Kotori

**Galgame 启动器 · 多设备存档同步 · 运行时缩放**

Linux 与 Windows 双端，兼容 Linux ARM。一个可执行文件，把多设备之间搬存档和给老游戏加缩放这两件麻烦事收进同一个界面。

[![CI](https://github.com/Pianzh/Kotori/actions/workflows/ci.yml/badge.svg)](https://github.com/Pianzh/Kotori/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/rust-2024%20edition-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)

</div>

> **开发中**：当前为 v0.0.x，功能未完成，配置格式与行为可能不兼容地变更。项目主要由 AI 辅助开发，遇到问题请直接提 issue。

## 这是什么

Kotori 是一款面向 Galgame 的集中管理工具，Linux 与 Windows 双端，同时兼容 Linux ARM。

做它的起因很具体。游戏分散在好几台设备上，Windows 机器和装在手机、平板上的 ARM 环境之间搬存档非常痛苦：每次都要手动拷文件、对目录、改前缀，换一次机器就再折腾一遍。Linux 这边想给 wine 下的老游戏加缩放，也得先记住 gamescope 那一长串命令行参数，或者去翻别人的配置文件。

Kotori 把这两件事收进一个界面。游戏点一下就启动，进游戏前存档已经是最新的，退出后自动备份；缩放参数在界面里挑，游戏运行中还能改。

功能与交互形态参考 [Magpie](https://github.com/Blinue/Magpie)，这是项目一开始就定下的方向。与市面上常见的那些管理器不同，Kotori 刻意不做 gal 周边那一套：分类、封面、元数据、VNDB 条目都不在范围内。将来是否会加入其中一部分，尚未确定。

## 功能

### 游戏库

- 手动登记游戏：游戏根目录、可执行文件、启动参数、Wine prefix（留空则自动探测）
- 两种启动方式：由 Kotori 启动（可套用缩放）／ 直接启动 exe
- 自动追踪进程：不是从 Kotori 启动的那一局（双击图标、Steam、第三方启动器）也能被识别，游戏退出后照常上传存档
- 外置盘路径记作「盘号 + 盘内相对目录」，盘没挂载也不影响建档，解析时明确报「未挂载」
- 目录扫描批量导入：`kotori scan` 打印结果，`kotori add` 直接写进配置

### 存档同步

- 存档位置三种形态：Windows 令牌路径、相对游戏根目录、本机绝对路径
- 每条存档位置可配排除规则（`*.log`、`cache/` 之类）
- 退出即上传，启动前自动取回
- 每一次同步生成一个独立版本，云端存档管理页可浏览、回滚、删除任意版本
- 凭据三种存储，按严格程度挑选：主密码加密文件 → 系统密钥环 → 0600 明文文件（默认落点）

### 缩放（Linux）

- 内部分辨率与输出分辨率、缩放比例、算法（FSR / NIS / Integer / Bilinear）、锐度、帧率上限、全屏
- 高级设置里可以手写一整行 gamescope 参数，写了之后 Kotori 计算出的那套全部让位
- 游戏运行中可改比例、算法、锐度，界面与命令行调用的是同一套动作，且只作用于指定的那一局
- KDE Plasma 下可调游戏窗口尺寸与全屏，经 KWin 脚本完成

## 平台支持

| 平台 | 启动游戏 | 进程追踪与退出后上传 | 运行时缩放 | 云存档同步 |
|:---|:---:|:---:|:---:|:---:|
| Linux x86_64 | ✅ wine + gamescope | ✅ | ✅ | ✅ |
| Linux aarch64 | ✅ | ✅ | ⚠️ 未验证 | ✅ |
| Windows x64 | ✅ 直接启动 exe | ✅ | ❌ 交给 Magpie | ✅ |

Windows 上的缩放不归 Kotori 管，缩放由 Magpie 负责，Kotori 与它之间只有观察能力，没有控制接口。

## 运行环境

Linux 上需要这些外部程序。设置页的「环境检查」会逐项探测，缺哪个会说明影响哪项功能、给出安装方式。

| 依赖 | 用途 | 必需性 |
| --- | --- | :---: |
| `wine` | 启动 Windows 游戏（缩放与直启都要） | 必需 |
| `gamescope` ≥ 3.16 | 带缩放启动游戏 | 缩放启动必需 |
| `kopia` | 云同步引擎（默认） | 可选 |
| `rclone` | 云同步引擎（备选） | 可选 |
| `secret-tool`（libsecret） | 系统密钥环 | 可选 |
| `xdg-desktop-portal` + 后端 | 「浏览…」文件选择对话框 | 可选 |
| KWin（仅 KDE Plasma） | 运行时调整窗口尺寸与全屏 | 可选 |

两个同步引擎至少要有一个。云同步引擎的查找顺序：设置页里指定的路径 → `KOTORI_KOPIA` / `KOTORI_RCLONE` → Kotori 可执行文件同目录 → `PATH`。

## 安装

### Windows

从 [Releases](https://github.com/Pianzh/Kotori/releases) 下载 `kotori-windows-x64.zip`，解压到任意目录，双击 `kotori.exe`。

```
kotori.exe     主程序
kopia.exe      内置的同步引擎（Apache-2.0）
LICENSES/      两个程序的许可证原文
README.txt     包内说明
BUILD-INFO.txt 构建信息与 sha256
```

MSVC 运行库已静态链接进 `kotori.exe`，不需要另外安装 VC++ 可再发行组件。

### Linux

下载对应架构的包并解压，得到一个可执行文件加两个附属文件：

```bash
tar --zstd -xf kotori-linux-x86_64.tar.zst   # ARM 设备用 kotori-linux-aarch64.tar.zst
cd kotori-linux-x86_64
./kotori
```

不需要安装到系统目录。想用 `kotori` 这个命令，把它放进 `PATH` 里的任意目录（例如 `~/.local/bin`）即可。包里附带 `io.github.kotori.desktop`，复制到 `~/.local/share/applications/` 就出现在应用菜单里。

> 装桌面文件前先确认 `kotori` 已经在 `PATH` 中。GIO 找不到 `Exec` 字段指定的程序时会把整份文件当作不存在，菜单项会不报错地消失。

`BUILD-INFO.txt` 记录了 commit、构建时间、rustc 版本、glibc 版本和 `ldd` 输出。程序在目标机器上跑不起来时先看它。

## 快速开始

1. **添加游戏** —— 在「添加游戏」页选游戏根目录与可执行文件，需要的话填启动参数。Wine prefix 留空即自动探测。
2. **配置存档位置** —— 在单游戏设置的「存档位置」里添加条目，选择路径形态，把不该同步的内容写进排除规则。
3. **配置云同步** —— 在「云同步」页选择引擎（默认 kopia），填写 Backblaze B2 的 bucket 与 key ID。凭据默认存在 0600 权限的明文文件里。
4. **开始使用** —— 在游戏库里点「启动」。退出后自动上传，在另一台机器上启动同一款游戏前自动取回。

关闭界面不会停止后台服务，已启动的游戏不受影响。服务状态可在设置页查看，或用 `kotori status`。

## 命令行

不带子命令直接运行就是打开图形界面，Windows 上双击 `kotori.exe`、Linux 上点桌面图标走的是同一条路径。

| 命令 | 说明 |
|:---|:---|
| `kotori` | 打开图形界面，需要时顺带拉起后台服务 |
| `kotori status` | 查看后台服务状态与正在运行的会话 |
| `kotori list` | 列出已登记的游戏 |
| `kotori launch <id>` | 启动指定游戏 |
| `kotori scan <目录>` | 扫描目录并打印识别到的游戏 |
| `kotori add <目录>` | 扫描目录并把结果写入配置 |
| `kotori sync status` | 查看同步配置、缺失项与各游戏的上次同步时间 |
| `kotori sync now [id]` | 立即上传，不带 id 则同步全部 |
| `kotori sync versions <id>` | 列出云端为该游戏保存的版本 |
| `kotori sync cloud` | 列出云端已有的全部游戏，包括本机未安装的 |
| `kotori sync restore <id> [--version <版本>]` | 取回存档，不带 `--version` 取最新版 |
| `kotori sync test` | 校验凭据与 bucket |
| `kotori sync master-password` | 把凭据转存为主密码加密文件 |
| `kotori sync lock` | 忘记密钥，需要重新输入主密码 |
| `kotori scale <子命令>` | 运行时调整缩放，见下文 |
| `kotori reload` | 手工改过 `config.toml` 后重新加载 |
| `kotori shutdown` | 停止后台服务 |

`kotori daemon` 与 `kotori ui` 分别显式启动后台服务与界面。

## 存档路径怎么填

每条存档位置在录入时就选定形态，存储的形态决定了它能不能跨机器解析。

| 形态 | 含义 | 适用场景 |
| --- | --- | --- |
| `windows` | Wine prefix 内的 Windows 路径 | 存档在 AppData、文档、SAVEDGAMES 下 |
| `relative` | 相对游戏根目录 | 其余能用相对路径表达的位置 |
| `absolute` | 本机绝对路径 | 只在这台机器上有效，不参与跨平台映射 |

`windows` 形态请使用 `%APPDATA%`、`%USERPROFILE%`、`%SAVEDGAMES%` 这类令牌，不要写死用户名。不同 prefix 里的用户名不一致（普通 wine 用 Linux 账户名，Proton 通常是 `steamuser`），写死后换机器就解析不出来。

点「浏览…」选一个位置，类型会自动识别并改写，识别顺序为 relative → windows 令牌 → absolute。

## 云同步

### 存储与引擎

后端是 **Backblaze B2**，传输引擎二选一：

- **kopia**（默认）—— 内容寻址仓库，增量上传、自带去重与加密，支持精确回滚到任意版本，省空间。
- **rclone** —— 一版存档打成一个完整 zip，包内是明文，可用其他工具直接解开，全量上传。

两个引擎在同一个 bucket 里各写各的区域，互不写入。换引擎之后看不到对方的数据，也不会报错，因此多台机器必须使用同一个引擎。

kopia 仓库有一个默认密码 `kotori`，任何拿到该 bucket 的人都能解开。需要保护的话自行设置，所有机器填同一个值。

索引在 bucket 中只存一份，物理结构是「合并快照 + 未合并增量」，两台机器同时同步不会互相覆盖。平时读取本机缓存，一小时内不访问网络；只有手动刷新、手动深度扫描云端，以及后台的启动时一次与每小时一次会联网。

### 凭据

凭据不放进 `config.toml`，按以下顺序挑选：

1. 已存在主密码加密文件 —— 用户明确选择的方案
2. 系统密钥环在运行 —— Linux 上通过 Secret Service
3. 0600 权限的明文凭据文件 —— 默认落点

Windows 凭据管理器尚未接入，Windows 上使用明文文件，`%APPDATA%` 的用户 ACL 即等价于 0600。

### 云端操作

「云端存档」页管理 bucket，与本机是否安装该游戏无关。四个操作都会先弹出确认：

| 操作 | 结果 |
|:---|:---|
| 删除某个版本 | 只删该版本，本机存档不动 |
| 清空某款游戏 | 删掉该游戏的全部存档，保留游戏条目，之后的同步仍写入同一条目 |
| 删除游戏条目 | 连同该条目的全部存档一起删除，不留无主数据 |
| 恢复某个版本 | 用云端版本覆盖本机存档目录，不可撤销 |

## 缩放

Linux 上游戏由 gamescope 启动。缩放参数在单游戏设置的「缩放」页配置：内部分辨率与输出分辨率、缩放比例、算法、锐度、全屏、帧率上限。高级设置里手写的 gamescope 参数会完全取代以上全部配置，此时运行时缩放自动停用。

游戏运行过程中可以调整，命令行与界面是同一套动作，只作用于指定的那一局：

| 子命令 | 说明 |
|:---|:---|
| `kotori scale status` | 查看运行中的会话及其当前缩放参数 |
| `kotori scale fsr` / `nis` | 切换 FSR / NIS 上采样 |
| `kotori scale integer` / `linear` | 整数（最近邻）／ 双线性 |
| `kotori scale sharpness <±n>` | 调整锐度，正数为更锐 |
| `kotori scale up` / `down` | 缩放比例升 / 降一档 |
| `kotori scale toggle` / `reset` | 在缩放与 1:1 之间切换 ／ 直接回到 1:1 |
| `kotori scale fullscreen` | 切换游戏窗口全屏（仅 KDE） |

同时运行多款游戏时，追加会话 ID 可指定操作对象。

在平铺桌面（niri）下窗口尺寸不由 Kotori 决定，游戏会铺满显示器，输出分辨率仅影响缩放的计算方式。KDE Plasma 下窗口尺寸与全屏经 KWin 完成。

## 配置文件位置

配置文件名为 `config.toml`，位置按以下顺序挑选：

1. **便携模式** —— 与可执行文件同目录。整个目录复制到另一台机器即可使用。
2. **平台默认目录** —— Linux 为 `~/.config/kotori/`，Windows 为 `%APPDATA%\kotori\`。

两者都存在时便携模式优先。设置页可在两者之间切换，切换会保留当前配置，并把让位的那份重命名为 `.portable-bak`。

## 从源码构建

需要 Rust 2024 edition（1.85 及以上）。Linux 上还需要 xkbcommon、fontconfig、xcb、openssl 的开发库。

```bash
# Arch
sudo pacman -S --needed base-devel git libxcb libxkbcommon fontconfig openssl

# Debian / Ubuntu
sudo apt-get install -y libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libfontconfig-dev libssl-dev

git clone https://github.com/Pianzh/Kotori
cd Kotori
cargo build --release --locked
```

产物是单个可执行文件 `target/release/kotori`。Windows 上 MSVC 运行库通过 `.cargo/config.toml` 静态链接，无需额外配置。

## 开发

```bash
scripts/test.sh              # 全部测试
cargo clippy --all-targets   # 静态检查
```

`scripts/test.sh` 会先剥掉 `DISPLAY` 等环境变量再跑测试，因为无头环境才是需要被覆盖的场景。

界面测试不开窗口：整页渲染并断言没有元素宽于窗口，命中测试发送真实指针事件并检查事件落点。涉及真实 B2 的测试在 `tests/portable_ipc/` 下，通过 `KOTORI_KOPIA` 与 `KOTORI_REAL_RCLONE` 指定真实可执行文件后以 `--ignored` 运行。

## 已知限制

- aarch64 上的缩放未经充分验证，gamescope 在 ARM 上的表现有待观察，同步功能不受影响
- Windows 上不支持缩放，相关参数在界面上禁用
- 目录扫描不是添加游戏的主要路径，游戏以手动登记为主
- 云端的存档管理目前只有删除操作，回滚到历史版本可用，但历史版本本身还不能整理
- 项目处于 v0.0.x 阶段，接口与配置可能随时不兼容地变更

## 路线图

1. 完善云端存档管理能力
2. 接入 Magpie，补齐 Windows 端的缩放体验
3. 验证并修复 aarch64 上的缩放
4. 扫描算法、游戏计时等周边功能（是否加入待定）

## 许可证

[GPL-3.0](LICENSE)

Windows 发布包内置的 kopia 是独立程序，采用 Apache-2.0 许可证，原文在 `LICENSES/` 目录中。同步过程中使用的 [rclone](https://rclone.org/) 与 [kopia](https://kopia.io/) 均为各自项目的独立程序。
