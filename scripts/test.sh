#!/usr/bin/env bash
#
# 跑测试，并且**先把环境净一遍**。
#
# 为什么要有这个脚本，而不是各自手打 `cargo test`：
#
#   * **没有桌面才是真相**。有些失败只在无显示时出现 —— Slint 的测试后端是**按线程**
#     注册的（见 `ui/render/window_test`），本机有 DISPLAY 时它会悄悄退回 winit 兜底，
#     于是**本地全绿、CI 全红**。2026-09-16 真发生过一次：run #21 只挂了一个设置页
#     测试，本地怎么跑都是绿的（那次是靠人手动 `env -u DISPLAY …` 才复现的）。
#   * 手打就得靠人记得加那几个 `-u`，而"记得"不是一种机制。
#
# 所以规矩是：**CI 跑什么，本地就跑什么 —— 都从这个脚本走。**
#
# 用法：
#   scripts/test.sh                       # 全部测试
#   scripts/test.sh --release --locked    # 参数原样交给 cargo test
#   scripts/test.sh some_test_name        # 只跑一个
set -euo pipefail

cd "$(dirname "$0")/.."

# 沙箱里 ~/.cargo 是只读的，项目因此在工作区里留了一份独立缓存（见 .gitignore 里的
# `.cargo-home/`）。只在"没设过 CARGO_HOME、且那份缓存真的在"时才用它 —— CI 上由
# actions/cache 管着，别去凭空建一个空目录。
if [ -z "${CARGO_HOME:-}" ] && [ -d "$PWD/.cargo-home" ]; then
    export CARGO_HOME="$PWD/.cargo-home"
fi

# 一个"最贫瘠"的环境：没有 Wayland，也没有 X11。`-u` 对没设置的变量是安全的。
exec env -u DISPLAY -u WAYLAND_DISPLAY -u WAYLAND_SOCKET \
    cargo test --all-targets "$@"
