# kotori

跨平台的 Galgame 管理器。设计上 **Linux 全功能**（启动 + gamescope 缩放增强 + 云存档同步），
**Windows 只做存档同步**。

[![CI](https://github.com/Pianzh/kotori/actions/workflows/ci.yml/badge.svg)](https://github.com/Pianzh/kotori/actions/workflows/ci.yml)

> ⚠ **现状**：Windows 那一端**还没开始移植** —— IPC（Unix socket）、进程管理（`/proc`、进程组）、
> 凭据后端这三处目前只有 Unix 实现，所以现在只有 Linux 能编译、能跑测试。
> “某平台上哪些功能不存在”这件事**不在各处散写 `cfg!`**，而是准备收进一张**能力表**由 daemon 报给界面，
> 界面据此隐藏/禁用对应的组并写一行说明（见 `AGENTS.md` §2.2 #16，尚未实现）。

## 两条贯穿全局的硬约束

1. **一切游戏启动（GUI 与 CLI）都经 IPC 交给常驻的 daemon** —— daemon 是唯一的配置写者、
   唯一的凭据持有者。界面崩了不影响正在玩的游戏。
2. **用户数据不进 git** —— 游戏清单、挂载点、存档路径只存在 `~/.config/kotori/config.toml`；
   文档与测试里只用占位路径（`/games/demo/game.exe`）。

## 构建

需要 Rust（stable）。Linux 上还需要 xkbcommon / fontconfig / xcb：

```bash
# Debian / Ubuntu
sudo apt-get install -y libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libfontconfig-dev libssl-dev
# Arch
sudo pacman -S --needed libxcb libxkbcommon fontconfig openssl
```

```bash
cargo build --release
cargo run -- ui        # 图形界面（会自动拉起 daemon）
cargo run -- status    # daemon 状态与运行中的会话
cargo run -- shutdown  # 停 daemon（不影响正在玩的游戏）
```

提交前必跑（CI 也跑这三条，外加一个原生 aarch64 的 job）：

```bash
cargo test --all-targets
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
```

外部工具按需使用：`gamescope`（缩放）、`wine`（运行）、`rclone`（同步）、
`secret-tool`（可选的系统密钥环）。路径与它们全部可注入，见 `KOTORI_*` 环境变量。

## 字体

界面优先使用系统里已有的 **微软雅黑 UI / Segoe UI Variable / Segoe Fluent Icons**，
系统里没有时退回 fontconfig 能提供的开源字体。**仓库不内嵌、也不分发任何微软字体**
（微软字体不允许随发行版打包）。历史上内嵌过一份 17MB 的兜底字体，因为 GUI 换成 Slint 之后
它的注册接口没了、已无人引用，所以从历史里剔除了。

## 许可

[GPL-3.0](LICENSE)。图形界面使用 [Slint](https://slint.dev/)，其免费档即 GPLv3，与本项目的许可一致。
