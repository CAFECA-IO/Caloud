#!/usr/bin/env sh
# 重新產生 docs/images/*.svg（README 圖示）。需要 python3，無其他依賴。
set -e
cd "$(dirname "$0")"
for f in architecture signin_flow deploy_flow domain_flow mcp_flow; do python3 "$f.py"; done
echo "已更新 docs/images/*.svg"
