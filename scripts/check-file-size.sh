#!/usr/bin/env bash
# 源码文件行数检查 —— AGENTS.md 的两条线：
#   * **500 行是软线**：写代码时该守的规矩。越过它会被这条脚本点出来，但**不拦 CI**。
#   * **600 行是硬线**：越过就红（CI 的「文件大小红线」一步跑的就是这个脚本）。
#     "600 才红"不是"可以放手写到 599"：软线提醒是给人看的，不是给人无视的。
#
# 规矩仍然是"**只减不增**"：
#   * 出现基线之外的新超标文件（> 硬线）  → **失败**（唯一让脚本红的情形）
#   * 基线里的文件又长胖了                → 警告（改代码难免长几行，但要看在眼里）
#   * 基线里的文件被拆到硬线以下          → 提示把它从基线删掉，数量就此减少
#   * 落在软线与硬线之间                  → 软线提醒（不失败，但别当没看见）
#
# 基线是 `scripts/file-size-baseline.txt`（`行数<TAB>路径`），用 `--update` 重写。
# ⚠ `--update` 会拒绝"让基线变长"：新超标文件必须由人拆掉，不能靠更新基线洗白
#   （确实要放行时用 `--force`，并且应该知道自己在干什么）。
#
# ✅ **已接 CI**（2026-09-25）：基线在同一天清零（最后一个 713 行的
#   `scale/gamescope.rs` 拆成 490 + 248），这一步进了
#   `.github/workflows/verify.yml` 的 linux job（就是 x86_64 那一份，CI 与
#   Release 共用）。从这以后**任何**新的超硬线文件都会让 CI 红 —— 那一天我自己
#   就因为还没接这一步，一口气顶过四个文件（见 PLATFORMS.md）。
#
# 用法：
#   bash scripts/check-file-size.sh            # 检查（有新超标就退出码 1）
#   bash scripts/check-file-size.sh --update   # 重写基线（只保留仍超硬线的文件）
#   bash scripts/check-file-size.sh --list     # 只列出行数超软线的文件，不比较基线
#   bash scripts/check-file-size.sh --limit 5  # 只看前 5 条（代替 `| head`）

set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

SOFT=500    # AGENTS.md 的软线：越过就提醒，不拦
HARD=600    # AGENTS.md 的硬线：越过就红（CI 拦的就是它）
BASELINE="scripts/file-size-baseline.txt"

mode=check
limit=0
while [ $# -gt 0 ]; do
  case "$1" in
    --update) mode=update ;;
    --force)  mode=force-update ;;
    --list)   mode=list ;;
    # 只看前几行请用它，别用 `| head`：下游提前关掉管道时，bash 会给每个写入
    # 报一句"断开的管道"（无害，但刷屏）。
    --limit)  shift; limit="${1:-0}" ;;
    *) echo "用法: $0 [--update|--force|--list] [--limit N]" >&2; exit 2 ;;
  esac
  shift
done

# 该显示第几条了（`--limit` 用；0 = 不限）。
shown=0
want_more() {
  if [ "$limit" -gt 0 ] && [ "$shown" -ge "$limit" ]; then
    return 1
  fi
  shown=$((shown + 1))
  return 0
}

# 源码 = 我们自己写的 .rs 与 .slint。`target/`、第三方代码、本地文档都不在扫描范围。
mapfile -t files < <(find src tests -type f \( -name '*.rs' -o -name '*.slint' \) | LC_ALL=C sort)

declare -A lines=()
hard=()   # 越过硬线 —— 会被拦的那一批
soft=()   # 落在软线与硬线之间 —— 会被点名、但放行
for file in "${files[@]}"; do
  count=$(wc -l < "$file")
  lines["$file"]=$count
  if [ "$count" -gt "$HARD" ]; then
    hard+=("$file")
  elif [ "$count" -gt "$SOFT" ]; then
    soft+=("$file")
  fi
done

# 行数从大到小排：先看最该拆的。
sorted_hard=()
if [ "${#hard[@]}" -gt 0 ]; then
  mapfile -t sorted_hard < <(
    for file in "${hard[@]}"; do printf '%08d %s\n' "${lines[$file]}" "$file"; done |
      LC_ALL=C sort -rn | cut -d' ' -f2-
  )
fi
sorted_soft=()
if [ "${#soft[@]}" -gt 0 ]; then
  mapfile -t sorted_soft < <(
    for file in "${soft[@]}"; do printf '%08d %s\n' "${lines[$file]}" "$file"; done |
      LC_ALL=C sort -rn | cut -d' ' -f2-
  )
fi

# 超硬线标 `!!`，软线里标 `~ `：一眼能分出"这条会被拦"和"这条只是提醒"。
tag() {
  if [ "${lines[$1]}" -gt "$HARD" ]; then printf '!!'; else printf '~ '; fi
}

if [ "$mode" = list ]; then
  for file in "${sorted_hard[@]}"; do
    want_more || break
    printf '%s %5d  %s\n' "$(tag "$file")" "${lines[$file]}" "$file"
  done
  for file in "${sorted_soft[@]}"; do
    want_more || break
    printf '%s %5d  %s\n' "$(tag "$file")" "${lines[$file]}" "$file"
  done
  exit 0
fi

# ── 基线 ────────────────────────────────────────────────────────────────────
declare -A base=()
if [ -f "$BASELINE" ]; then
  while IFS=$'\t' read -r count file; do
    case "$count" in ''|'#'*) continue ;; esac
    base["$file"]=$count
  done < "$BASELINE"
fi

if [ "$mode" = update ] || [ "$mode" = force-update ]; then
  # 基线还不存在时是首次生成，不算"变长"。
  if [ "$mode" = update ] && [ -f "$BASELINE" ] && [ "${#hard[@]}" -gt "${#base[@]}" ]; then
    echo "拒绝更新基线：条目会从 ${#base[@]} 涨到 ${#hard[@]}。" >&2
    echo "新超标的那几个文件得先拆，或者用 --force 明确放行。" >&2
    exit 1
  fi
  {
    echo "# 超过硬线 $HARD 行的文件。规矩是只减不增：这个列表只允许变短。"
    echo "# 由 scripts/check-file-size.sh --update 生成，别手改。"
    for file in "${sorted_hard[@]}"; do
      printf '%s\t%s\n' "${lines[$file]}" "$file"
    done
  } > "$BASELINE"
  echo "基线已更新：${#hard[@]} 个文件超过硬线 $HARD 行（原 ${#base[@]} 个）→ $BASELINE"
  exit 0
fi

# ── 检查 ────────────────────────────────────────────────────────────────────
added=()    # 基线外的新超标文件 —— 唯一让脚本红的东西
grown=()    # 基线内，但比基线记录的行数还多
shrunk=()   # 基线内，已经拆到硬线以下（或文件没了）
for file in "${hard[@]}"; do
  if [ -z "${base[$file]:-}" ]; then
    added+=("$file")
  elif [ "${lines[$file]}" -gt "${base[$file]}" ]; then
    grown+=("$file")
  fi
done
for file in "${!base[@]}"; do
  if [ "${lines[$file]:-0}" -le "$HARD" ]; then
    shrunk+=("$file")
  fi
done

printf '超过硬线 %d 行的源文件：%d 个（基线 %d 个）；另有 %d 个落在软线 %d 行之上\n' \
  "$HARD" "${#hard[@]}" "${#base[@]}" "${#soft[@]}" "$SOFT"
for file in "${sorted_hard[@]}"; do
  want_more || break
  note=""
  if [ -n "${base[$file]:-}" ] && [ "${lines[$file]}" -gt "${base[$file]}" ]; then
    note="  ← 比基线多了 $((lines[$file] - base[$file])) 行"
  fi
  printf '%s %5d  %s%s\n' "$(tag "$file")" "${lines[$file]}" "$file" "$note"
done
[ "${#hard[@]}" -eq 0 ] && echo "（一个都没有，硬线守住了）"

if [ "${#soft[@]}" -gt 0 ]; then
  echo
  echo "~ 已经越过软线 $SOFT 行（不拦，但下一次改动该考虑拆了）："
  for file in "${sorted_soft[@]}"; do
    want_more || break
    printf '  %5d  %s\n' "${lines[$file]}" "$file"
  done
fi

if [ "${#shrunk[@]}" -gt 0 ]; then
  echo
  echo "已经拆到硬线 $HARD 行以下，跑 --update 把它们从基线里去掉："
  for file in "${shrunk[@]}"; do
    printf '  - %s\n' "$file"
  done
fi

if [ "${#grown[@]}" -gt 0 ]; then
  echo
  echo "⚠ 这些文件比基线里记的还长（数量没变，但确实在变胖）："
  for file in "${grown[@]}"; do
    printf '  %s: %d → %d\n' "$file" "${base[$file]}" "${lines[$file]}"
  done
fi

if [ "${#added[@]}" -gt 0 ]; then
  echo
  echo "✗ 出现基线之外的新超标文件 —— 本次改动就该顺手拆掉它："
  for file in "${added[@]}"; do
    printf '  %5d  %s\n' "${lines[$file]}" "$file"
  done
  echo
  echo "（真要放行：bash scripts/check-file-size.sh --force）"
  exit 1
fi

exit 0
