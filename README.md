# kotori

## 依赖

kotori 自己是一个 Rust 二进制,但它编排的是**系统里已有的**游戏与同步栈 —— 下面这些东西
不在包内,必须由系统提供:

| 依赖 | 没有它就没有 | 必需性 |
| --- | --- | --- |
| `gamescope` ≥ 3.16 | 启动游戏、缩放增强 | **必需**(不装就没法启动游戏) |
| `wine` | 启动游戏 | **必需**(只管理原生 Linux 游戏则不需要) |
| `rclone` | 云存档同步 | **必需**(不用云同步则不需要) |
| `xdg-desktop-portal` + 一个后端 | 各处「浏览…」按钮调出的系统文件对话框 | 可选:探不到就把按钮灰掉并写明理由 |
| KWin(仅 KDE Plasma) | 运行时改变游戏窗口尺寸 | 可选:平铺桌面(niri)上如实回「做不到」 |
| `kscreen-doctor`(KDE)/`niri` | 探测输出分辨率 | 可选:没有就退回环境变量与内置默认 |
| `secret-tool`(libsecret) | 系统密钥环 | 可选:没有就用权限 0600 的明文凭据文件(这是默认行为) |

## 从源码构建

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

## 安装

```bash
sudo install -Dm755 target/release/kotori /usr/local/bin/kotori
install -Dm644 assets/io.github.kotori.desktop \
  ~/.local/share/applications/io.github.kotori.desktop
```

⚠ 桌面文件要**在 kotori 已经位于 PATH 上之后再装**:GIO 找不到 `Exec` 里那个程序时会
把整份文件当作不存在,**静默**不显示菜单项。

装好在应用菜单里搜 kotori,或者直接:

```bash
kotori ui        # 图形界面(会自动拉起守护进程)
kotori status    # 守护进程状态与正在运行的会话
```
