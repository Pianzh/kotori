#!/usr/bin/env bash
#
# 把"刻意不入主仓"的本地文档同步到本机的那份备份克隆。
#
# 为什么需要这个脚本:这些文档**只在主仓之外活着**(见 `.gitignore`),而它们装着这个项目
# 绝大部分的"为什么"。代码丢了能从 GitHub 回来,这些丢了就真没了。
#
# 用法:
#   scripts/notes-sync.sh                          # 同步到 .notes-target 里写的那个克隆
#   KOTORI_NOTES_DIR=/path/to/notes scripts/notes-sync.sh
#   scripts/notes-sync.sh --check                  # 只跑闸门,不复制也不提交
#
# ⚠ 备份的那一头是**只读镜像**:要改文档就改主仓里的原件,再跑这个脚本同步过去,
#   别在镜像里直接编辑(否则两边会分叉,而分叉的备份等于没有备份)。
#
# ⚠ 这个脚本本身在 GitHub 上,所以它里面**不写**任何仓名、组织名、平台名,也不写备份
#   克隆的路径 —— 路径通常就是按仓名起的,写进来等于把名字带出去。这些东西放本机:
#   `.notes-target` 一行路径(被 `.git/info/exclude` 排除),规则表见下面。
#
# ⚠ 推之前有一道**闸门**(见下面"规则表"):拦的是"不该公开出现"的字符串。规则表也放
#   本机(`.leak-rules`,被 `.git/info/exclude` 排除)—— 规则本身就是要藏的东西。
set -euo pipefail

SRC="$(cd "$(dirname "$0")/.." && pwd)"
MODE="${1:-sync}"
MODE="${MODE#--}"   # `--check` 与 `check` 都认

# 备份克隆在哪:本机一个小文件,一行路径。**不写在这个脚本里**(见文件头)。
TARGET="${KOTORI_NOTES_TARGET:-$SRC/.notes-target}"
DEST="${KOTORI_NOTES_DIR:-}"
if [ -z "$DEST" ] && [ -f "$TARGET" ]; then
    DEST="$(sed -n '1{s/[[:space:]]*$//;p;}' "$TARGET")"
fi

# `.gitignore` 里那几个"故意不入主仓"的文件。将来再加,记得也加在这儿。
FILES=(
    AGENTS.md
    ARCHITECTURE.md
    DISCIPLINE.md
    GOALS.md
    HANDOVER.md
    PITFALLS.md
    PLATFORMS.md
    SUBAGENT.md
    STRUCTURE.md
    UI_GUIDE.md
)

# ── 闸门 ────────────────────────────────────────────────────────────────────
# 每行一个子串,大小写不敏感;`#` 开头与空行忽略。
# 规则表不在、或者一条规则都没有,一律**拒绝** —— 闸门失效时不该默默放行。
DENY="${KOTORI_LEAK_RULES:-$SRC/.leak-rules}"
if [ ! -f "$DENY" ]; then
    cat >&2 <<'EOF'
✗ 找不到闸门的规则表

这个脚本要拦的字符串**刻意不写在这里** —— 写进脚本,等于让脚本自己去泄露。
规则表就是一个文本文件:每行一个要拦的子串,大小写不敏感,`#` 开头是注释。
它被 `.git/info/exclude` 排除,不进 git;从本机的安全副本恢复一份。
要临时换一份:KOTORI_LEAK_RULES=/path/to/list $0
EOF
    exit 1
fi

PATTERNS=()
while IFS= read -r pat; do
    case "$pat" in ''|'#'*) continue ;; esac
    PATTERNS+=("$pat")
done < "$DENY"
if [ "${#PATTERNS[@]}" -eq 0 ]; then
    echo "✗ 规则表 $DENY 里一条规则都没有 —— 空闸门等于没有闸门" >&2
    exit 1
fi

leak=0

# 命中时只报"第几条规则",**不把规则本身打出来** —— 那正是要藏的东西。
# `-m 2` 而不是 `| head -2`:管道被下游提前关掉时 `pipefail` 会让这一行以 141 退出,
# 抢在下面那句"拒绝"之前把脚本带走。
scan_file() {   # $1=来源标签 $2=文件
    local where="$1" file="$2" i=0 pat
    [ -f "$file" ] || return 0
    for pat in "${PATTERNS[@]}"; do
        i=$((i + 1))
        if grep -Fiq -- "$pat" "$file"; then
            echo "✗ [$where] $file 命中规则表第 $i 条:" >&2
            grep -Fin -m 2 -- "$pat" "$file" | sed 's/^/    /' >&2
            leak=1
        fi
    done
}

scan_text() {   # $1=来源标签 $2=一段文本
    local where="$1" text="$2" i=0 pat
    for pat in "${PATTERNS[@]}"; do
        i=$((i + 1))
        if printf '%s\n' "$text" | grep -Fiq -- "$pat"; then
            echo "✗ [$where] 命中规则表第 $i 条:" >&2
            printf '%s\n' "$text" | grep -Fin -m 2 -- "$pat" | sed 's/^/    /' >&2
            leak=1
        fi
    done
}

# ① 要同步过去的那几份文档 —— 它们整份都在主仓之外活着。
for f in "${FILES[@]}"; do
    scan_file "本地文档" "$SRC/$f"
done

# ② 已经入了 git 的文件 —— 它们本来就要公开,而**历史是删不掉的**:只改最新一版没用,
#    早先那个版本在 GitHub 上照样按 commit 取得到。这一条才是真正咬过人的地方。
tracked_n=0
while IFS= read -r f; do
    [ -n "$f" ] || continue
    tracked_n=$((tracked_n + 1))
    scan_file "入 git 的文件" "$SRC/$f"
done < <(git -C "$SRC" ls-files)

# ③ 还没入 git、但也没被忽略的新文件 —— 下一次 `git add .` 就会带上它们。
while IFS= read -r f; do
    [ -n "$f" ] || continue
    scan_file "未入 git 的新文件" "$SRC/$f"
done < <(git -C "$SRC" ls-files --others --exclude-standard)

# ④ 还没推出去的提交说明 —— 说明和代码一样公开,而且一样删不掉。
if git -C "$SRC" rev-parse --verify -q '@{upstream}' >/dev/null 2>&1; then
    msgs="$(git -C "$SRC" log --format='%h %s%n%b' '@{upstream}..HEAD')"
    if [ -n "$msgs" ]; then
        scan_text "未推的提交说明" "$msgs"
    fi
fi

if [ "$leak" -ne 0 ]; then
    cat >&2 <<'EOF'

拒绝:上面这些字符串不该出现在要公开的地方。
要么把它们从文档/代码里去掉,要么把那点内容挪回本机(不进 git)。
EOF
    exit 1
fi
echo "✓ 闸门通过:${#FILES[@]} 份本地文档 + $tracked_n 个入 git 的文件都没命中规则表"
if [ "$MODE" = check ]; then
    exit 0
fi

if [ ! -d "$DEST/.git" ]; then
    cat >&2 <<EOF
✗ 找不到备份克隆:"\${DEST:-（没配置）}"

这个脚本**不写备份远端的名字,也不写克隆在哪**(写了等于把它们带出去)。
先把克隆放在本机某个地方,再把那一行路径写进:
  $TARGET
或者临时用环境变量指过去:
  KOTORI_NOTES_DIR=/path/to/notes \$0
EOF
    exit 1
fi

missing=0
for f in "${FILES[@]}"; do
    if [ -f "$SRC/$f" ]; then
        cp "$SRC/$f" "$DEST/$f"
    else
        echo "⚠ 主仓里没有 $f,跳过" >&2
        missing=1
    fi
done
[ "$missing" -eq 0 ] || echo "(有文件没同步成功,检查上面的警告)" >&2

cd "$DEST"
git add -A
if git diff --cached --quiet; then
    echo "没有变化,不用提交"
    exit 0
fi

git commit -q -m "notes: sync from $(hostname 2>/dev/null || echo 本机) $(date '+%Y-%m-%d %H:%M')"
git push -q origin HEAD
echo "已同步并推送:$(git log --oneline -1)"
