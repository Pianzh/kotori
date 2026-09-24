#!/usr/bin/env bash
#
# 下载**固定版本**的官方 kopia 二进制并校验官方 checksums.txt,把可执行文件路径
# 写进 KOTORI_KOPIA(通过 $GITHUB_ENV,CI 里下一步就能直接用)。verify.yml 的
# Linux 臂靠它养那四个真 kopia 的 ignored 测试。
#
# 用法:
#   scripts/install-kopia.sh [版本] [安装目录]
#     版本     默认 0.23.1 —— 必须与 release.yml 的 KOPIA_VERSION 对齐
#     安装目录 默认 $PWD/kopia-dl
#
# 输出:
#   * 解压后的 kopia 可执行文件路径写入 $GITHUB_ENV 的 KOTORI_KOPIA(有 GITHUB_ENV 时)
#   * 同时把路径打到 stdout,方便本地手动调试
set -euo pipefail

version="${1:-0.23.1}"
dest="${2:-$PWD/kopia-dl}"
mkdir -p "$dest"
dest="$(cd "$dest" && pwd)"

base="https://github.com/kopia/kopia/releases/download/v$version"
asset="kopia-$version-linux-x64.tar.gz"

cd "$dest"
curl -fsSL -o "$asset" "$base/$asset"
curl -fsSL -o checksums.txt "$base/checksums.txt"

# 官方 checksums.txt 一行一个 "<sha256>  <文件名>",也可能写成二进制模式的
# "<sha256>  *<文件名>",只取第一段。匹配不到直接停,别放行未校验的下载。
want="$(awk -v a="$asset" '$2 == a || $2 == "*" a { print $1; exit }' checksums.txt)"
if [ -z "$want" ]; then
    echo "错误: checksums.txt 里没有 $asset" >&2
    exit 1
fi
got="$(sha256sum "$asset" | awk '{print $1}')"
if [ "$got" != "$want" ]; then
    echo "错误: kopia 校验失败: 期望 $want, 实际 $got" >&2
    exit 1
fi
echo "ok: $asset sha256 校验通过"

tar -xzf "$asset"
rm -f "$asset" checksums.txt

# tar 里是一个带版本号的目录,别假设层级,递归找可执行的 kopia。
bin="$(find "$dest" -type f -name kopia -perm -u+x | head -1)"
if [ -z "$bin" ]; then
    echo "错误: 解压出来的包里没有 kopia 可执行文件" >&2
    exit 1
fi

if [ -n "${GITHUB_ENV:-}" ]; then
    echo "KOTORI_KOPIA=$bin" >> "$GITHUB_ENV"
fi
echo "$bin"