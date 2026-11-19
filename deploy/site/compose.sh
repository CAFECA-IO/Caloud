#!/bin/sh
# 產生 caloud.tw 形象頁的 compose（App「caloud-tw-site」，綁定 caloud.tw 與 www.caloud.tw）。
# 頁面與 nginx 設定壓縮後放進環境變數，容器啟動時寫出；不需要建置映像，也不掛主機路徑。
#   sh deploy/site/compose.sh > /tmp/site-compose.yml   # 再以 update_app／管理介面更新 compose 並重新部署
set -eu
dir=$(dirname "$0")
enc() { gzip -9c "$1" | base64 | tr -d '\n'; }
cat <<YAML
# caloud.tw 形象頁：原始檔在 Caloud 儲存庫 deploy/site/（index.html、nginx.conf）
services:
  web:
    image: nginx:1.27-alpine
    environment:
      PAGE_GZ_B64: "$(enc "$dir/index.html")"
      CONF_GZ_B64: "$(enc "$dir/nginx.conf")"
    command: ["sh", "-c", "echo \\"\$\$PAGE_GZ_B64\\" | base64 -d | gunzip > /usr/share/nginx/html/index.html && echo \\"\$\$CONF_GZ_B64\\" | base64 -d | gunzip > /etc/nginx/conf.d/default.conf && exec nginx -g 'daemon off;'"]
YAML
