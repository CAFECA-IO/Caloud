#!/usr/bin/env bash
# Runner 主機設定（ADR 0015、FR-OPS-11）：gVisor runtime 與環境防火牆。
#
#   sudo scripts/runner-setup.sh install     # 安裝並生效（不重啟既有容器）
#   sudo scripts/runner-setup.sh status      # 顯示目前狀態
#   sudo scripts/runner-setup.sh uninstall   # 完整移除
#
# 函式名稱以 do_ 開頭，避免遮蔽同名指令（例如 install）。
# 只動 Caloud 自己的檔案與 chain：
#   /usr/local/lib/caloud/gvisor/            runsc 與 gvisor-bin/
#   /etc/docker/daemon.json                  只合併 runtimes.runsc（預設 runtime 不變）
#   iptables CALOUD-ENV-FWD（DOCKER-USER）、CALOUD-ENV-IN（INPUT）
#   /etc/systemd/system/caloud-firewall.service  開機還原規則
set -euo pipefail

GVISOR_VERSION="20261116.0"
GVISOR_SHA256="495796a56d811a43ea218bd0af3d89ea471df1cbd684edcb21c4a00356caeb1e"
GVISOR_URL="https://storage.googleapis.com/gvisor/releases/pool/${GVISOR_VERSION}/binary-amd64/runsc.deb"
PREFIX=/usr/local/lib/caloud
RUNSC="$PREFIX/gvisor/runsc"
DAEMON_JSON=/etc/docker/daemon.json
MARKER="$PREFIX/daemon-json-created"
UNIT=/etc/systemd/system/caloud-firewall.service
# 環境網段介面（ADR 0015：bridge 名稱 cld<索引>）
IFACE="cld+"
BLOCKED="10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 169.254.0.0/16 100.64.0.0/10"

need_root() { [ "$(id -u)" = 0 ] || { echo "請以 root 執行（sudo）。" >&2; exit 1; }; }

# ---------------------------------------------------------------- 防火牆
firewall_up() {
  for c in CALOUD-ENV-FWD CALOUD-ENV-IN; do iptables -N "$c" 2>/dev/null || iptables -F "$c"; done
  iptables -A CALOUD-ENV-FWD -i "$IFACE" -o "$IFACE" -j RETURN
  iptables -A CALOUD-ENV-FWD -i "$IFACE" -m conntrack --ctstate ESTABLISHED,RELATED -j RETURN
  for n in $BLOCKED; do iptables -A CALOUD-ENV-FWD -i "$IFACE" -d "$n" -j DROP; done
  iptables -A CALOUD-ENV-FWD -i "$IFACE" -p tcp --dport 25 -j DROP
  iptables -A CALOUD-ENV-IN -i "$IFACE" -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
  iptables -A CALOUD-ENV-IN -i "$IFACE" -j DROP
  iptables -N DOCKER-USER 2>/dev/null || true
  iptables -C DOCKER-USER -j CALOUD-ENV-FWD 2>/dev/null || iptables -I DOCKER-USER 1 -j CALOUD-ENV-FWD
  iptables -C INPUT -j CALOUD-ENV-IN 2>/dev/null || iptables -I INPUT 1 -j CALOUD-ENV-IN
}

firewall_down() {
  while iptables -D DOCKER-USER -j CALOUD-ENV-FWD 2>/dev/null; do :; done
  while iptables -D INPUT -j CALOUD-ENV-IN 2>/dev/null; do :; done
  for c in CALOUD-ENV-FWD CALOUD-ENV-IN; do iptables -F "$c" 2>/dev/null || true; iptables -X "$c" 2>/dev/null || true; done
}

# ---------------------------------------------------------------- daemon.json
# 合併或移除 runtimes.runsc；其他設定原樣保留
daemon_json() {
  python3 - "$1" "$DAEMON_JSON" "$RUNSC" <<'PY'
import json, os, sys
action, path, runsc = sys.argv[1], sys.argv[2], sys.argv[3]
cfg = {}
if os.path.exists(path):
    with open(path) as f:
        text = f.read().strip()
        cfg = json.loads(text) if text else {}
rt = cfg.setdefault("runtimes", {})
if action == "add":
    rt["runsc"] = {"path": runsc}
else:
    rt.pop("runsc", None)
tmp = path + ".caloud-tmp"
with open(tmp, "w") as f:
    json.dump(cfg, f, indent=2)
    f.write("\n")
os.replace(tmp, path)
PY
}

do_install() {
  need_root
  command -v docker >/dev/null && command -v iptables >/dev/null && command -v python3 >/dev/null \
    || { echo "需要 docker、iptables、python3。" >&2; exit 1; }
  [ "$(uname -m)" = x86_64 ] || { echo "目前只支援 x86_64。" >&2; exit 1; }
  local tmp; tmp=$(mktemp -d)
  # shellcheck disable=SC2064
  trap "rm -rf '$tmp'" EXIT
  echo "▸ 下載 gVisor ${GVISOR_VERSION}"
  curl -fsSL -o "$tmp/runsc.deb" "$GVISOR_URL"
  echo "${GVISOR_SHA256}  $tmp/runsc.deb" | sha256sum -c --quiet
  dpkg-deb -x "$tmp/runsc.deb" "$tmp/x"
  mkdir -p "$PREFIX/gvisor"
  install -m 755 "$tmp/x/usr/bin/runsc" "$tmp/x/usr/bin/containerd-shim-runsc-v1" "$PREFIX/gvisor/"
  rm -rf "$PREFIX/gvisor/gvisor-bin"; cp -r "$tmp/x/usr/bin/gvisor-bin" "$PREFIX/gvisor/"; chmod -R a+rx "$PREFIX/gvisor"
  echo "▸ 註冊 Docker runtime runsc（reload，不重啟容器）"
  mkdir -p "$(dirname "$DAEMON_JSON")"
  [ -e "$DAEMON_JSON" ] || touch "$MARKER"
  daemon_json add
  systemctl reload docker
  echo "▸ 環境防火牆"
  firewall_up
  cat > "$UNIT" <<UNIT
[Unit]
Description=Caloud Runner environment firewall (ADR 0015)
After=docker.service
Wants=docker.service

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=$PREFIX/runner-setup.sh firewall-up
ExecStop=$PREFIX/runner-setup.sh firewall-down

[Install]
WantedBy=multi-user.target
UNIT
  install -m 755 "$0" "$PREFIX/runner-setup.sh"
  systemctl daemon-reload
  systemctl enable caloud-firewall.service >/dev/null 2>&1
  echo "✓ 完成。用 caloud-agent --self-check 驗證。"
}

do_uninstall() {
  need_root
  systemctl disable caloud-firewall.service >/dev/null 2>&1 || true
  rm -f "$UNIT"; systemctl daemon-reload
  firewall_down
  if [ -e "$DAEMON_JSON" ]; then
    # 先移除 runsc 並 reload（直接刪檔再 reload 不會清掉已註冊的 runtime）
    daemon_json remove
    systemctl reload docker
    if [ -e "$MARKER" ]; then
      python3 -c 'import json,sys; c=json.load(open(sys.argv[1])); c.get("runtimes") == {} and c.pop("runtimes"); sys.exit(0 if c == {} else 1)' "$DAEMON_JSON" \
        && rm -f "$DAEMON_JSON"
    fi
  fi
  rm -rf "$PREFIX"
  echo "✓ 已移除。"
}

do_status() {
  echo "runsc:        $([ -x "$RUNSC" ] && "$RUNSC" --version | head -1 || echo 未安裝)"
  echo "docker 註冊:   $(docker info --format '{{range $k, $v := .Runtimes}}{{$k}} {{end}}' 2>/dev/null)"
  echo "防火牆 FWD:    $(iptables -S CALOUD-ENV-FWD 2>/dev/null | grep -c -- '-A' || true) 條"
  echo "防火牆 IN:     $(iptables -S CALOUD-ENV-IN 2>/dev/null | grep -c -- '-A' || true) 條"
  echo "開機還原:      $(systemctl is-enabled caloud-firewall.service 2>/dev/null || echo 未安裝)"
}

case "${1:-}" in
  install) do_install ;;
  uninstall) do_uninstall ;;
  status) do_status ;;
  firewall-up) need_root; firewall_up ;;
  firewall-down) need_root; firewall_down ;;
  *) echo "用法：$0 install|status|uninstall" >&2; exit 2 ;;
esac
