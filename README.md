# kotori

由 Rust 写的全平台 Galgame 管理器,更侧重于解决 Linux 端的缩放问题,提供了简单的云同步服务,并兼容 ARM。

## 依赖

### Linux

kotori 自己是一个 Rust 二进制,但它编排的是**系统里已有的**游戏与同步栈 —— 下面这些东西
不在包内,必须由系统提供:

| 依赖 | 没有它就没有 | 必需性 |
| --- | --- | --- |
| `gamescope` ≥ 3.16 | 启动游戏、缩放增强 | **必需**(不装就没法启动游戏) |
| `wine` | 启动游戏 | **必需**(只管理原生 Linux 游戏则不需要) |
| `kopia` | 云存档同步(默认引擎) | **默认**:切到 rclone 引擎则不需要 |
| `rclone` | 云存档同步(备选引擎) | 可选:只有选了 rclone 引擎才需要 |
| `xdg-desktop-portal` + 一个后端 | 各处「浏览…」按钮调出的系统文件对话框 | 可选:探不到就把按钮灰掉并写明理由 |
| KWin(仅 KDE Plasma) | 运行时改变游戏窗口尺寸 | 可选:平铺桌面(niri)上如实回「做不到」 |
| `secret-tool`(libsecret) | 系统密钥环 | 可选:没有就用权限 0600 的明文凭据文件(这是默认行为) |

### Windows

Windows 版不负责启动游戏,包里已经带齐了运行所需 —— 基本不用装东西:

| 依赖 | 没有它就没有 | 必需性 |
| --- | --- | --- |
| `kopia` | 云存档同步(默认引擎) | **无需安装**:已内置在包里,放在 `kotori.exe` 旁边,kotori 自己会找到它 |
| `rclone` | 云存档同步(备选引擎) | 可选:只有选了 rclone 引擎才需要 |

## 安装

### Windows

从 GitHub Release 下载 `kotori-windows-x64.zip`,解压到任意目录即用。包里是:

```
kotori.exe     主程序(双击直接开界面)
kopia.exe      内置的同步引擎
LICENSES/      两个程序的许可证原文
README.txt     包内说明
```

### Linux:从源码构建

需要 Rust(2024 edition,即 1.85 以上)。Linux 上还要 xkbcommon / fontconfig / xcb 的开发库:

```bash
# Arch
sudo pacman -S --needed base-devel git
sudo pacman -S --needed libxcb libxkbcommon fontconfig openssl

# Debian / Ubuntu
sudo apt-get install -y libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libfontconfig-dev libssl-dev

git clone https://github.com/Pianzh/kotori
cd kotori
cargo build --release
```

### Linux:安装

```bash
sudo install -Dm755 target/release/kotori /usr/local/bin/kotori
install -Dm644 assets/io.github.kotori.desktop \
  ~/.local/share/applications/io.github.kotori.desktop
```

⚠ 桌面文件要**在 kotori 已经位于 PATH 上之后再装**:GIO 找不到 `Exec` 里那个程序时会
把整份文件当作不存在,**静默**不显示菜单项。

## 启动

`kotori` **不给子命令就是启动 UI** —— Windows 双击 `kotori.exe`、Linux 点桌面图标或
直接敲 `kotori`,走的都是同一条路。`kotori --help` 照旧列出其余子命令。

```bash
kotori        # 启动图形界面(会自动拉起守护进程)
kotori status # 守护进程状态与正在运行的会话
```

## 其他问题

本项目高度依赖 AI 编写,目前仍在开发中,可能有大量未验证的 bug,并可能随时破坏性更新。

ARM 端的缩放功能还没有实现。

