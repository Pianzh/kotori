#!/usr/bin/env bash
#
# 上传前的扫描器:只扫**这次要送出去的东西**,不扫工作区。
#
# 为什么范围是"要送出去的东西":工作区干净不等于历史干净。2026-09-17 那次泄露的不是
# 文件本身,是"文件 + 提交说明" —— 它们在 GitHub 上按 commit 取得到,改掉最新一版没用。
# 所以这里扫三样:
#   ① 这次要送的每个提交的说明(含正文)
#   ② 这次要送的每个 blob 的内容(按对象去重,不重复扫)
#   ③ 送完之后远端那棵树的样子 —— 规则表刚加一条时,老内容照样中招
#
# 规则有两个来源:
#   * 通用密钥样式:写死在下面,不含任何项目私事,所以它可以公开
#   * 本机规则表 `.leak-rules`(不进 git):组织名/仓名/平台名这类,**必须存在**
# 另可配一份本机密钥清单 `.leak-secrets`:直接搜"那些密钥的值本身"
#
# 用法:
#   scripts/leak-scan.sh                 # pre-push 钩子的用法:从 stdin 读 ref 对
#   scripts/leak-scan.sh --range A..B    # 手动扫一个区间
#   scripts/leak-scan.sh --tree [REV]    # 只扫 REV(默认 HEAD)的整棵树
#   scripts/leak-scan.sh --history       # 扫 HEAD 可达的全部历史(慢,审计用)
#   scripts/leak-scan.sh --install-hook  # 把 pre-push 钩子装进本仓
#
# 真要跳过:`git push --no-verify`(明确跳过,别拿它当默认)。
set -euo pipefail

SRC="$(cd "$(dirname "$0")/.." && pwd)"
g() { git -C "$SRC" "$@"; }

GITDIR="$(g rev-parse --git-dir)"
case "$GITDIR" in /*) ;; *) GITDIR="$SRC/$GITDIR" ;; esac
TMP="$GITDIR/leak-scan.$$"
mkdir -p "$TMP"
trap 'rm -rf "$TMP"' EXIT

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
# 找不到就**拒绝**:今天这场泄露有一半原因就是"闸门只认两个名字",不能再来一次。
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
}

scan_range() {   # $1=区间表达式 $2=要顺带扫树的那次提交(可空) 其余=额外 rev-list 参数
    local range="$1" head="$2" sha otype osha opath size
    shift 2

    # ① 提交说明:一个提交一个临时文件,这样报错能指名道姓
    while IFS= read -r sha; do
        n_commits=$((n_commits + 1))
        g log -1 --format='%B' "$sha" > "$TMP/msg"
        scan_file "提交说明" "提交 ${sha:0:7}" "$TMP/msg"
    done < <(g rev-list "$range" "$@")

    # ② 要送出去的 blob(`rev-list --objects` 列的就是这次要传的对象)
    while IFS=' ' read -r otype osha opath; do
        [ "$otype" = "blob" ] || continue
        n_blobs=$((n_blobs + 1))
        size="$(g cat-file -s "$osha" 2>/dev/null || echo 0)"
        if [ "$size" -gt 2097152 ]; then
            [ -z "$opath" ] || scan_path_rule "新增内容" "$opath"
            continue
        fi
        g cat-file blob "$osha" > "$TMP/blob"
        if [ -n "$opath" ]; then scan_file "新增内容" "$opath" "$TMP/blob"
        else scan_file "新增内容" "blob ${osha:0:12}" "$TMP/blob"; fi
    done < <(g rev-list --objects "$range" "$@" \
                | g cat-file --batch-check='%(objecttype) %(objectname) %(rest)' 2>/dev/null || true)

    # ③ 送完之后远端那棵树
    [ -n "$head" ] || return 0
    scan_tree "$head"
}

scan_tree() {   # $1=提交
    local rev="$1" f
    rm -rf "$TMP/tree"
    mkdir -p "$TMP/tree"
    g archive "$rev" 2>/dev/null | tar -x -C "$TMP/tree" 2>/dev/null || true
    while IFS= read -r f; do
        scan_file "整棵树" "${f#"$TMP/tree/"}" "$f"
    done < <(find "$TMP/tree" -type f)
}

report() {
    if [ "$leak" -ne 0 ]; then
        cat >&2 <<'EOF'

✗ 拦下了:上面这些命中不许送出去。这次 push 什么都没传。
  把内容改干净再推;确认是误报再用 `git push --no-verify`。
EOF
        exit 1
    fi
    echo "✓ 上传前扫描通过:$n_commits 个提交的说明、$n_blobs 个对象、$n_files 个文件都没命中"
}

mode="${1:-pre-push}"
case "$mode" in --*) mode="${mode#--}" ;; esac

case "$mode" in
    range)
        r="${2:?用法: leak-scan.sh --range A..B}"
        scan_range "$r" "$(g rev-parse "${r##*..}")"
        report
        ;;
    tree)
        scan_tree "$(g rev-parse "${2:-HEAD}")"
        report
        ;;
    history)
        scan_range "HEAD" "HEAD"
        report
        ;;
    install-hook)
        hp="$(g config --get core.hooksPath || true)"
        if [ -n "$hp" ]; then
            echo "✗ 这个仓设了 core.hooksPath=$hp,钩子要装到那儿去" >&2
            exit 1
        fi
        hookfile="$GITDIR/hooks/pre-push"
        cat > "$hookfile" <<EOF
#!/usr/bin/env bash
# 由 scripts/leak-scan.sh --install-hook 装的:push 之前先扫要送出去的对象。
# 钩子自己的参数(<远端名> <地址>)这里用不上,要读的是 stdin 上的 ref 对。
exec "$SRC/scripts/leak-scan.sh" --hook
EOF
        chmod +x "$hookfile"
        echo "✓ 已装:$hookfile"
        ;;
    hook|pre-push)
        # 钩子的输入:每行 `<本地 ref> <本地 sha> <远端 ref> <远端 sha>`
        while read -r _lref lsha _rref rsha; do
            [ -n "${lsha:-}" ] || continue
            [ "$lsha" = "0000000000000000000000000000000000000000" ] && continue   # 删分支
            if [ "$rsha" = "0000000000000000000000000000000000000000" ] || [ -z "${rsha:-}" ]; then
                scan_range "$lsha" "$lsha" --not --remotes
            else
                scan_range "$rsha..$lsha" "$lsha"
            fi
        done
        report
        ;;
    *)
        echo "用法: leak-scan.sh [--range A..B | --tree [REV] | --history | --install-hook]" >&2
        exit 2
        ;;
esac
