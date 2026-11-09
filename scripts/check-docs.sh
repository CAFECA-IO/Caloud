#!/usr/bin/env bash
# 檢查程式碼是否遵守 docs/ 的規範（按圖施工）。規則見 docs/README.md「文件治理」。
#
#   1. 程式與文件中引用的需求編號（FR-/NFR-）都定義在 01-requirements.md
#   2. 01 定義的每個 FR 都出現在 03-development-plan.md 的工作項目
#   3. caloud-api 的每個 API 路徑都列在 06-api-spec.md
#   4. web/ 內所有檔案使用的每個 CSS 自訂屬性（Design Token）都列在 04-design-language.md
#   5. web/ 內所有檔案使用的每個 class 都列在 05-components.md；index.html 不得自帶樣式
#   6. compose 政策的每個禁用鍵都列在 07-security.md
#   7. docs/ 內的相對連結都指向存在的檔案
#   8. web/ds/theme.css 的淺色與兩個深色區塊都完整定義語意 Token
#   9. MCP 工具（crates/caloud-api/src/mcp.rs）與 06-api-spec.md §5.3 的工具表一一對應
#
# 相容 macOS 內建 bash 3.2 與 Linux。
set -u
cd "$(dirname "$0")/.."

DOCS=docs
REQ=$DOCS/01-requirements.md
PLAN=$DOCS/03-development-plan.md
DESIGN=$DOCS/04-design-language.md
COMP=$DOCS/05-components.md
API=$DOCS/06-api-spec.md
SEC=$DOCS/07-security.md
WEB=web/index.html
ROUTER=crates/caloud-api/src/lib.rs
POLICY=crates/caloud-orchestrator/src/policy.rs

fail=0
err() { echo "  ✗ $*"; fail=1; }
section() { echo "▸ $*"; }

# 展開「FR-USER-01～04」這類範圍為逐一編號
expand_ids() {
  grep -ohE 'N?FR-[A-Z0-9]+-[0-9]{2}(～[0-9]{2})?' "$@" 2>/dev/null | awk -F'～' '{
    if (NF == 2) {
      prefix = substr($1, 1, length($1) - 2); start = substr($1, length($1) - 1) + 0; end = $2 + 0
      for (i = start; i <= end; i++) printf "%s%02d\n", prefix, i
    } else print $1
  }' | sort -u
}

# ---------------------------------------------------------------- 1
section "需求編號皆有定義（$REQ）"
defined=$(grep -oE '^\| N?FR-[A-Z0-9]+-[0-9]{2} ' "$REQ" | sed 's/^| //; s/ $//' | sort -u)
used_files=$(find crates web docs -type f \( -name '*.rs' -o -name '*.html' -o -name '*.md' -o -name '*.sql' \) ! -path "$REQ" ! -path '*/adr/0000-template.md')
# shellcheck disable=SC2086
for id in $(expand_ids $used_files); do
  echo "$defined" | grep -qx "$id" || err "未定義的需求編號 $id（請先加入 $REQ）"
done

# ---------------------------------------------------------------- 2
section "每個功能需求都有工作項目（$PLAN）"
planned=$(expand_ids "$PLAN")
for id in $(echo "$defined" | grep '^FR-'); do
  echo "$planned" | grep -qx "$id" || err "$id 沒有對應的工作項目（請加入 $PLAN）"
done

# ---------------------------------------------------------------- 3
section "API 路徑皆列於 $API"
for path in $(grep -oE '"/api/[^"]*"' "$ROUTER" | tr -d '"' | sort -u); do
  grep -qF "\`$path\`" "$API" || err "API 路徑 $path 未列在 $API"
done

# ---------------------------------------------------------------- 4
WEB_FILES=$(find web -type f \( -name '*.html' -o -name '*.css' -o -name '*.js' \) | sort)
section "Design Token 皆列於 $DESIGN"
# 只看 Token 的用法：var(--x)、定義 --x:、字串 "--x"（不把 CLI 參數 --header 之類當成 Token）
# shellcheck disable=SC2086
for tok in $(grep -ohE -- 'var\(--[a-z][a-z0-9-]*|(^|[^a-zA-Z0-9_-])--[a-z][a-z0-9-]*[[:space:]]*:|"--[a-z][a-z0-9-]*"' $WEB_FILES | grep -oE -- '--[a-z][a-z0-9-]*' | sort -u); do
  grep -qF "\`$tok\`" "$DESIGN" || err "Token $tok 未定義於 $DESIGN"
done

# ---------------------------------------------------------------- 5
section "CSS class 皆列於 $COMP"
# 所有 c-／l-／u-／is-／sg- 開頭的識別字（CSS 選擇器、class 屬性、JS 字串）都必須是文件定義的 class
# shellcheck disable=SC2086
missing=$(grep -ohE '(^|[^a-zA-Z0-9_-])(c|l|u|is|sg)-[a-z0-9]+([_-]{1,2}[a-z0-9]+)*' $WEB_FILES | sed -E 's/^[^a-z]//' | sort -u | while read -r cls; do
  grep -qF "\`$cls\`" "$COMP" || echo "$cls"
done)
for cls in $missing; do err "class .$cls 未定義於 $COMP"; done
if grep -nE '<style|[[:space:]]style="' "$WEB" >/dev/null; then err "$WEB 不得有 <style> 或 style 屬性（docs/05 §A.1）"; fi

# ---------------------------------------------------------------- 6
section "compose 政策規則皆列於 $SEC"
for list in FORBIDDEN_KEYS NAMESPACE_KEYS; do
  keys=$(awk -v l="$list" '$0 ~ "const " l {f=1} f {print} f && /\];/ {exit}' "$POLICY" | grep -oE '"[a-z_]+"' | tr -d '"')
  [ -z "$keys" ] && err "找不到 $list"
  for k in $keys; do
    grep -qF "| \`$k\` |" "$SEC" || err "$list 的 $k 未列於 $SEC"
  done
done

# ---------------------------------------------------------------- 7
section "文件相對連結有效"
for f in $(find "$DOCS" -name '*.md' ! -name '0000-template.md') README.md CONTRIBUTING.md AGENTS.md; do
  [ -f "$f" ] || continue
  dir=$(dirname "$f")
  for link in $(grep -oE '\]\([^)#: ]+\.md' "$f" | sed 's/^](//'); do
    [ -f "$dir/$link" ] || err "$f 連結到不存在的 $link"
  done
done

# ---------------------------------------------------------------- 8
THEME=web/ds/theme.css
section "$THEME 完整定義語意 Token（$DESIGN §2.2–2.3）"
color_tokens=$(grep -oE '^\| `--(color|shadow)-[a-z0-9-]+`' "$DESIGN" | tr -d '|` ' | sort -u)
style_tokens=$(grep -oE '^\| `--(font|radius|control|brand)-[a-z0-9-]+`' "$DESIGN" | tr -d '|` ' | sort -u)
# 依 :root 區塊切開：1 淺色、2 跟隨系統深色、3 data-theme="dark"
for n in 1 2 3; do
  defined=$(awk -v n="$n" '/:root/{b++} b==n && /^[[:space:]]*--/{sub(/^[[:space:]]*/,""); sub(/:.*/,""); print} /^[[:space:]]*}/{if(b==n) exit}' "$THEME" | sort -u)
  need="$color_tokens"
  [ "$n" = 1 ] && need="$need $style_tokens"
  for t in $need; do
    echo "$defined" | grep -qx -- "$t" || err "$THEME 第 $n 個區塊缺少 $t"
  done
done

# ---------------------------------------------------------------- 9
MCP=crates/caloud-api/src/mcp.rs
section "MCP 工具與 $API §5.3 一致"
code_tools=$(tr '\n' ' ' < "$MCP" | grep -oE 'tool\([[:space:]]*"[a-z0-9_]+"' | grep -oE '"[a-z0-9_]+"' | tr -d '"' | sort -u)
doc_tools=$(awk '/^### 5\.3/{f=1;next} /^##/{f=0} f' "$API" | grep -oE '^\| `[a-z0-9_]+` \|' | tr -d '|` ' | sort -u)
[ -z "$code_tools" ] && err "在 $MCP 找不到工具定義"
for t in $code_tools; do echo "$doc_tools" | grep -qx "$t" || err "MCP 工具 $t 未列在 $API §5.3"; done
for t in $doc_tools; do echo "$code_tools" | grep -qx "$t" || err "$API §5.3 的工具 $t 沒有實作"; done

echo
if [ "$fail" -ne 0 ]; then
  echo "文件遵循檢查失敗：請依 docs/README.md 的規則先更新文件，或修正程式碼。"
  exit 1
fi
echo "文件遵循檢查通過。"
