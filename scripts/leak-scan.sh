#!/usr/bin/env bash
#
# 上传前的扫描器:在本机、在最早的时刻、把所有分支都扫一遍。
#
# 为什么这么做:
#   * 推出去的东西 = 永久公开。重写历史只删 ref 不删对象,悬空提交照样能按 SHA 匿名
#     取到。所以**唯一安全的时刻是推之前**,唯一安全的位置是本机。
#   * 工作区干净不等于历史干净;本分支干净不等于别的分支干净 —— 泄露会从"另一个分支
#     将来被合并/误推"这条路走回来。
#   * 扫的是"对象",不是文件:提交说明(含正文)、每个 blob 的内容。说明是最容易漏的
#     地方(2026-09-17 有两次就栽在说明和注释里)。
#
# 三个钩子(都靠 `--install-hook` 装,钩子文件本身没法进版本库):
#   commit-msg   写说明的那一刻就扫说明          —— 毫秒级
#   pre-commit   提交前扫暂存区                  —— 百毫秒级
#   pre-push     推之前扫**所有可能被推出去的东西** —— 全量十几秒,换"永远真扫"
#
# 规则有两个来源:
#   * 通用密钥样式:写死在下面,不含任何项目私事,所以它可以公开
#   * 本机规则表 `.leak-rules`(不进 git):认得出你的字符串,**必须存在**
# 另可配一份本机密钥清单 `.leak-secrets`:直接搜"那些密钥的值本身"
#
# 用法:
#   scripts/leak-scan.sh --install-hook     # 装/更新三个钩子
#   scripts/leak-scan.sh --all              # 本机**所有** refs(含挪出 refs/heads 的草稿),审计用
#   scripts/leak-scan.sh --range A..B       # 只扫一个区间(提交前的预览)
#   scripts/leak-scan.sh --tree [REV]       # 只扫 REV(默认 HEAD)的整棵树
#   scripts/leak-scan.sh --staged           # 只扫暂存区
#   scripts/leak-scan.sh --commit-msg FILE  # 只扫一份提交说明
#
# 真要跳过:`git push --no-verify` / `git commit --no-verify`(明确跳过,别当默认)。
set -euo pipefail

SRC="$(cd "$(dirname "$0")/.." && pwd)"
g() { git -C "$SRC" "$@"; }

GITDIR="$(g rev-parse --git-dir)"
case "$GITDIR" in /*) ;; *) GITDIR="$SRC/$GITDIR" ;; esac
TMP="$GITDIR/leak-scan.$$"
mkdir -p "$TMP"
trap 'rm -rf "$TMP"' EXIT

# 超过这个大小的对象只查文件名,不读内容(读它没意义,还慢)。跳过的数量会报出来,
# 不默默略过。
BIG=8388608

# ── 通用密钥样式 ────────────────────────────────────────────────────────────
# 名字只用来报错;命中时**不打印内容** —— 免得密钥被复制到别的地方去。
RE_NAME=(
    "GitHub 令牌"
    "GitHub 细粒度令牌"
    "AWS 访问密钥"
    "私钥文件内容"
    "Slack 令牌"
    "OpenAI 风格密钥"
    "JWT"
    "写死的口令/令牌字面量"
)
RE_PAT=(
    'gh[pousr]_[A-Za-z0-9]{36}'
    'github_pat_[A-Za-z0-9_]{20,}'
    'AKIA[0-9A-Z]{16}'
    '-----BEGIN [A-Z ]*PRIVATE KEY-----'
    'xox[baprs]-[A-Za-z0-9-]{10,}'
    'sk-[A-Za-z0-9]{32,}'
    'eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}'
    "(token|password|passwd|secret|api[_-]?key|access[_-]?key|applicationkey)[[:space:]]*[:=][[:space:]]*[\"'][^\"']{16,}[\"']"
)

# 文件名本身就说明问题的(只比 basename)。仓库里要是真有这种测试夹具,改这条。
PATH_RE='^(\.env(\..+)?|.*\.(pem|key|p12|pfx|jks|kdbx|ppk)|id_(rsa|dsa|ecdsa|ed25519).*|\.netrc|.*\.password)$'

# ── 本机规则表 ──────────────────────────────────────────────────────────────
# 每行一条:普通行按**子串**(大小写不敏感),`re:` 开头当正则。空行与 `#` 忽略。
# 找不到就**拒绝**:闸门失效时放行,正是上一次出事的方式。
RULES="${KOTORI_LEAK_RULES:-$SRC/.leak-rules}"
if [ ! -f "$RULES" ]; then
    cat >&2 <<EOF
✗ 找不到本机规则表:$RULES

没有它,只有通用密钥样式在拦 —— 组织名、仓名、平台名这类"认得出你"的字符串没人管。
这个文件**刻意不进 git**:规则本身就是要藏的东西。补一个(每行一条,`re:` 当正则),
或者明确跳过这一次:
  git push --no-verify
EOF
    exit 1
fi

LOCAL_FIXED=()
LOCAL_RE=()
while IFS= read -r line; do
    case "$line" in ''|'#'*) continue ;; esac
    case "$line" in
        re:*) LOCAL_RE+=("${line#re:}") ;;
        *) LOCAL_FIXED+=("$line") ;;
    esac
done < "$RULES"
if [ "${#LOCAL_FIXED[@]}" -eq 0 ] && [ "${#LOCAL_RE[@]}" -eq 0 ]; then
    echo "✗ 规则表 $RULES 里一条规则都没有 —— 空闸门等于没有闸门" >&2
    exit 1
fi

# 批量匹配用:规则落成文件,`grep -f` 一次过,比按条 grep 快一个量级。
: > "$TMP/fixed"
: > "$TMP/re"
[ "${#LOCAL_FIXED[@]}" -eq 0 ] || printf '%s\n' "${LOCAL_FIXED[@]}" >> "$TMP/fixed"
[ "${#LOCAL_RE[@]}" -eq 0 ] || printf '%s\n' "${LOCAL_RE[@]}" >> "$TMP/re"
printf '%s\n' "${RE_PAT[@]}" >> "$TMP/re"

# ── 本机密钥清单(可选):搜"值本身" ─────────────────────────────────────────
# 每行一个字面量,或者 `file:<路径>`。从文件里抽候选值时只认"长得像密钥的":
# 全是 [A-Za-z0-9+/_=-] 且长度 ≥ 16,并且字母数字都有 —— 这样路径、桶名、纯数字 id
# 不会被当成密钥(它们会天天误报)。
SECRETS="${KOTORI_LEAK_SECRETS:-$SRC/.leak-secrets}"
SECRET_VALUES=()
if [ -f "$SECRETS" ]; then
    while IFS= read -r line; do
        case "$line" in ''|'#'*) continue ;; esac
        case "$line" in
            file:*)
                f="${line#file:}"
                f="${f/#\~/$HOME}"
                [ -f "$f" ] || continue
                while IFS= read -r v; do
                    v="${v#"${v%%[![:space:]]*}"}"   # 去头尾空白
                    v="${v%"${v##*[![:space:]]}"}"
                    case "$v" in ''|'#'*) continue ;; esac
                    case "$v" in *:*) v="${v#*:}" ;; *=*) v="${v#*=}" ;; esac
                    v="${v#"${v%%[![:space:]]*}"}"
                    v="${v%"${v##*[![:space:]]}"}"
                    v="${v#\"}"; v="${v%\"}"; v="${v#\'}"; v="${v%\'}"
                    if printf '%s' "$v" | grep -qE '^[A-Za-z0-9+/_=-]{16,}$' &&
                       printf '%s' "$v" | grep -q '[0-9]' &&
                       printf '%s' "$v" | grep -q '[A-Za-z]'; then
                        SECRET_VALUES+=("$v")
                    fi
                done < "$f"
                ;;
            *) SECRET_VALUES+=("$line") ;;
        esac
    done < "$SECRETS"
fi
: > "$TMP/secrets"
[ "${#SECRET_VALUES[@]}" -eq 0 ] || printf '%s\n' "${SECRET_VALUES[@]}" >> "$TMP/secrets"

# ── 扫描 ────────────────────────────────────────────────────────────────────
leak=0
n_commits=0
n_blobs=0
n_files=0
n_big=0

# 命中"通用样式/本机正则/密钥值"时报是哪一条 —— 只用于内部判断,不外传内容
attribution_of() {
    local text="$1" k
    for ((k = 0; k < ${#LOCAL_RE[@]}; k++)); do
        if printf '%s' "$text" | grep -qiE -- "${LOCAL_RE[$k]}"; then
            printf '本机规则(正则)第 %d 条' "$((k + 1))"
            return 0
        fi
    done
    for ((k = 0; k < ${#RE_PAT[@]}; k++)); do
        if printf '%s' "$text" | grep -qiE -- "${RE_PAT[$k]}"; then
            printf '「%s」样式' "${RE_NAME[$k]}"
            return 0
        fi
    done
    printf '规则表'
}

scan_path_rule() {   # $1=标签 $2=显示名
    if printf '%s' "${2##*/}" | grep -qE -- "$PATH_RE"; then
        echo "✗ [$1] $2:这个文件名本身就不该进仓" >&2
        leak=1
    fi
    return 0
}

scan_file() {   # $1=标签 $2=显示名(可空) $3=真实路径
    local tag="$1" name="$2" path="$3" hit ln text where k
    [ -f "$path" ] || return 0
    n_files=$((n_files + 1))

    # ① 本机规则(子串):把命中的原样贴出来 —— 这类是要人去改的名字
    while IFS= read -r hit; do
        ln="${hit%%:*}"; text="${hit#*:}"
        if [ -n "$name" ]; then where="$name 第 $ln 行"; else where="第 $ln 行"; fi
        for ((k = 0; k < ${#LOCAL_FIXED[@]}; k++)); do
            if printf '%s' "$text" | grep -qiF -- "${LOCAL_FIXED[$k]}"; then
                printf '✗ [%s] %s 命中本机规则第 %d 条:\n' "$tag" "$where" "$((k + 1))" >&2
                printf '      %.200s\n' "$text" >&2
                leak=1
                break
            fi
        done
    done < <(grep -naFi -f "$TMP/fixed" -- "$path" || true)

    # ② 通用样式 + 本机正则 + 本机密钥值:只说位置,**不贴内容**
    while IFS= read -r hit; do
        ln="${hit%%:*}"; text="${hit#*:}"
        if [ -n "$name" ]; then where="$name 第 $ln 行"; else where="第 $ln 行"; fi
        printf '✗ [%s] %s 命中%s(内容不贴出来)\n' "$tag" "$where" "$(attribution_of "$text")" >&2
        leak=1
    done < <(grep -naiE -f "$TMP/re" -- "$path" || true)

    while IFS= read -r hit; do
        ln="${hit%%:*}"
        if [ -n "$name" ]; then where="$name 第 $ln 行"; else where="第 $ln 行"; fi
        printf '✗ [%s] %s 命中本机密钥清单里的一枚密钥(内容不贴出来)\n' "$tag" "$where" >&2
        leak=1
    done < <(grep -naF -f "$TMP/secrets" -- "$path" || true)

    [ -z "$name" ] || scan_path_rule "$tag" "$name"
    return 0
}

scan_blob() {   # $1=标签 $2=路径(可空) $3=对象 sha
    local size
    size="$(g cat-file -s "$3" 2>/dev/null || echo 0)"
    if [ "$size" -gt "$BIG" ]; then
        n_big=$((n_big + 1))
        [ -z "$2" ] || scan_path_rule "$1" "$2"
        return 0
    fi
    g cat-file blob "$3" > "$TMP/blob"
    if [ -n "$2" ]; then scan_file "$1" "$2" "$TMP/blob"
    else scan_file "$1" "blob ${3:0:12}" "$TMP/blob"; fi
}

scan_commits() {   # 其余参数转给 rev-list
    local sha
    while IFS= read -r sha; do
        n_commits=$((n_commits + 1))
        g log -1 --format='%B' "$sha" > "$TMP/msg"
        scan_file "提交说明" "提交 ${sha:0:7}" "$TMP/msg"
    done < <(g rev-list "$@")
}

scan_blobs() {   # 其余参数转给 rev-list;`--objects` 列的正是这些对象
    local otype osha opath
    while IFS=' ' read -r otype osha opath; do
        [ "$otype" = "blob" ] || continue
        n_blobs=$((n_blobs + 1))
        scan_blob "内容" "$opath" "$osha"
    done < <(g rev-list --objects "$@" \
                | g cat-file --batch-check='%(objecttype) %(objectname) %(rest)' 2>/dev/null || true)
}

scan_tree() {   # $1=提交
    local f
    rm -rf "$TMP/tree"
    mkdir -p "$TMP/tree"
    g archive "$1" 2>/dev/null | tar -x -C "$TMP/tree" 2>/dev/null || true
    while IFS= read -r f; do
        scan_file "整棵树" "${f#"$TMP/tree/"}" "$f"
    done < <(find "$TMP/tree" -type f)
}

scan_staged() {   # 暂存区(逐行解析 `diff --cached --raw`,路径在 TAB 之后)
    local line meta path sha
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        meta="${line%%$'\t'*}"; path="${line#*$'\t'}"
        sha="$(printf '%s' "$meta" | awk '{print $4}')"
        case "$sha" in ""|0000000000000000000000000000000000000000) continue ;; esac
        n_blobs=$((n_blobs + 1))
        scan_blob "暂存区" "$path" "$sha"
    done < <(g diff --cached --raw)
}

report() {
    local extra=""
    [ "$n_big" -eq 0 ] || extra="(另有 $n_big 个超过 8MB 的对象只查了文件名)"
    if [ "$leak" -ne 0 ]; then
        cat >&2 <<'EOF'

✗ 拦下了:上面这些命中不许送出去。
  把内容改干净再来;确认是误报再用 `--no-verify` 明确跳过。
EOF
        exit 1
    fi
    echo "✓ 扫描通过:$n_commits 个提交的说明、$n_blobs 个对象、$n_files 个文件都没命中$extra"
}

# ── 钩子 ────────────────────────────────────────────────────────────────────
install_hook() {   # $1=钩子名 $2=内容
    local f="$GITDIR/hooks/$1" tmp="$GITDIR/hooks/.$1.tmp.$$"
    if [ -e "$f" ] && ! grep -q 'leak-scan.sh' "$f" 2>/dev/null; then
        echo "✗ $f 已经存在,而且不是我们装的 —— 先自己看一眼,别盖掉" >&2
        return 1
    fi
    # 写临时文件再 `mv`(原子替换)。直接 `> $f` 是"截断 + 写":另一个 agent 正好在这时
    # push/commit,会读到被截断甚至空的钩子 —— 空脚本以 0 退出,等于闸门**静默失效**。
    printf '%s' "$2" > "$tmp"
    chmod +x "$tmp"
    mv -f "$tmp" "$f"
    echo "✓ $f"
}

mode="${1:-all}"
case "$mode" in --*) mode="${mode#--}" ;; esac
shift || true

case "$mode" in
    all)
        scan_commits --all
        scan_blobs --all
        report
        ;;
    range)
        r="${1:?用法: leak-scan.sh --range A..B}"
        scan_commits "$r"
        scan_blobs "$r"
        scan_tree "$(g rev-parse "${r##*..}")"
        report
        ;;
    tree)
        scan_tree "$(g rev-parse "${1:-HEAD}")"
        report
        ;;
    staged)
        scan_staged
        report
        ;;
    commit-msg)
        f="${1:?用法: leak-scan.sh --commit-msg <文件>}"
        [ -f "$f" ] || { echo "✗ 没有这个文件:$f" >&2; exit 1; }
        scan_file "提交说明" "本次提交" "$f"
        report
        ;;
    hook)
        # pre-push:不只扫这次要推的,而是**所有可能被推出去的东西** —— 别的分支上藏着的,
        # 将来一样会被合并或误推出去。集合 = 本地分支/标签/远端跟踪 + 这次要推的 SHA
        # (后者连"拿一个游离对象顶上去"这种歪路也算上)。
        #
        # 注意:`--all` 是更宽的审计视角(连 refs/scrap 那种挪出 refs/heads 的草稿都算),
        # 所以它可能报出你**根本没打算推**的东西 —— 那是有意的,别把两个模式搞混。
        refs=()
        while IFS= read -r r; do
            [ -n "$r" ] && refs+=("$r")
        done < <(g for-each-ref --format='%(refname)' refs/heads refs/tags refs/remotes)
        while read -r _lref lsha _rref _rsha; do
            [ -n "${lsha:-}" ] || continue
            [ "$lsha" = "0000000000000000000000000000000000000000" ] && continue
            refs+=("$lsha")
        done
        if [ "${#refs[@]}" -eq 0 ]; then
            echo "✓ 没有可推的东西"
            exit 0
        fi
        scan_commits "${refs[@]}"
        scan_blobs "${refs[@]}"
        report
        ;;
    install-hook)
        hp="$(g config --get core.hooksPath || true)"
        if [ -n "$hp" ]; then
            echo "✗ 这个仓设了 core.hooksPath=$hp,钩子要装到那儿去" >&2
            exit 1
        fi
        install_hook pre-push "#!/usr/bin/env bash
# 由 scripts/leak-scan.sh --install-hook 装的:推之前扫**所有本地分支**的说明与内容。
# 钩子自己的参数(<远端名> <地址>)这里用不上,要读的是 stdin 上的 ref 对。
exec \"$SRC/scripts/leak-scan.sh\" --hook
"
        install_hook commit-msg "#!/usr/bin/env bash
# 由 scripts/leak-scan.sh --install-hook 装的:写说明的那一刻就扫说明。
exec \"$SRC/scripts/leak-scan.sh\" --commit-msg \"\$1\"
"
        install_hook pre-commit "#!/usr/bin/env bash
# 由 scripts/leak-scan.sh --install-hook 装的:提交前扫暂存区。
exec \"$SRC/scripts/leak-scan.sh\" --staged
"
        cat <<'EOF'

⚠ 钩子文件没法进版本库(这是 git 的设计):换机器、换克隆要再跑一次 --install-hook。
EOF
        ;;
    *)
        echo "用法: leak-scan.sh [--all | --range A..B | --tree [REV] | --staged | --commit-msg FILE | --install-hook]" >&2
        exit 2
        ;;
esac
