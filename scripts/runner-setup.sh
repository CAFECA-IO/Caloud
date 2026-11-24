#!/usr/bin/env bash
# Runner 主機設定（ADR 0015、ADR 0018、FR-OPS-11）：gVisor runtime、環境防火牆、建置環境與 agent 服務。
#
#   sudo scripts/runner-setup.sh install [--agent <caloud-agent 執行檔>] [--server <控制面網址> --join <加入權杖>]
#                                            # 安裝並生效（不重啟既有容器）；有 --server 時安裝並啟動 agent 服務
#   sudo scripts/runner-setup.sh status      # 顯示目前狀態
#   sudo scripts/runner-setup.sh uninstall   # 完整移除（含 agent 服務、節點權杖、建置網路與映像）
#
# 函式名稱以 do_ 開頭，避免遮蔽同名指令（例如 install）。
# 只動 Caloud 自己的檔案與 chain：
#   /usr/local/lib/caloud/gvisor/            runsc 與 gvisor-bin/
#   /etc/docker/daemon.json                  只合併 runtimes.runsc 與（有 GPU 時）runtimes.runsc-gpu（預設 runtime 不變）
#   有 NVIDIA GPU 時（ADR 0022）：NVIDIA 官方 apt 套件 nvidia-container-toolkit（提供 Docker --gpus 的 hook）
#   iptables CALOUD-ENV-FWD（DOCKER-USER）、CALOUD-ENV-IN（INPUT）
#   /etc/systemd/system/caloud-firewall.service  開機還原規則
#   docker 網路 caloud-build（bridge cldbuild，10.239.0.0/24）與 BuildKit 映像（ADR 0018）
#   /etc/systemd/system/caloud-agent.service、/etc/caloud/agent.env、/var/lib/caloud-agent/
set -euo pipefail

GVISOR_VERSION="20261116.0"
GVISOR_SHA256="495796a56d811a43ea218bd0af3d89ea471df1cbd684edcb21c4a00356caeb1e"
GVISOR_URL="https://storage.googleapis.com/gvisor/releases/pool/${GVISOR_VERSION}/binary-amd64/runsc.deb"
PREFIX=/usr/local/lib/caloud
RUNSC="$PREFIX/gvisor/runsc"
DAEMON_JSON=/etc/docker/daemon.json
MARKER="$PREFIX/daemon-json-created"
UNIT=/etc/systemd/system/caloud-firewall.service
AGENT_UNIT=/etc/systemd/system/caloud-agent.service
AGENT_ENV=/etc/caloud/agent.env
AGENT_STATE=/var/lib/caloud-agent
# 建置與驗證沙箱（ADR 0018、ADR 0020）：介面名稱符合 cld+，沿用環境防火牆
BUILD_NET=caloud-build
BUILD_SUBNET=10.239.0.0/24
BUILDKIT_IMAGE="moby/buildkit@sha256:5987568f92dafbfb679879de59c1cd8d26597f5f6635c314e166164e2615870c"
# 驗證的相依套件漏洞掃描（ADR 0020）：osv-scanner v2.3.0
OSV_IMAGE="ghcr.io/google/osv-scanner@sha256:6421cdd773a54fad5fd991895b4f3840beba55a2f30a8182185919e13979a2c8"
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
# 合併或移除 runtimes.runsc／runsc-gpu；其他設定原樣保留
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
elif action == "add-gpu":
    # ADR 0022：同一個 runsc，帶 --nvproxy；只給宣告 GPU 的服務使用
    rt["runsc-gpu"] = {"path": runsc, "runtimeArgs": ["--nvproxy"]}
elif action == "remove-gpu":
    rt.pop("runsc-gpu", None)
else:
    rt.pop("runsc", None)
    rt.pop("runsc-gpu", None)
tmp = path + ".caloud-tmp"
with open(tmp, "w") as f:
    json.dump(cfg, f, indent=2)
    f.write("\n")
os.replace(tmp, path)
PY
}

# ---------------------------------------------------------------- GPU（ADR 0022）
NV_KEYRING=/usr/share/keyrings/nvidia-container-toolkit-keyring.gpg
NV_LIST=/etc/apt/sources.list.d/nvidia-container-toolkit.list

gpu_driver() { nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null | head -1 | tr -d ' '; }

# 有 NVIDIA GPU 時：安裝 nvidia-container-toolkit、註冊 runsc-gpu；驅動不在 gVisor 支援清單時略過
gpu_setup() {
  if ! command -v nvidia-smi >/dev/null || [ -z "$(gpu_driver)" ]; then
    echo "▸ GPU：未偵測到 NVIDIA GPU，略過"
    daemon_json remove-gpu
    return 0
  fi
  local driver; driver=$(gpu_driver)
  if ! "$RUNSC" nvproxy list-supported-drivers 2>/dev/null | grep -qx "$driver"; then
    echo "▸ GPU：驅動 $driver 不在 gVisor ${GVISOR_VERSION} 的 nvproxy 支援清單，略過（請更新驅動或 gVisor）" >&2
    daemon_json remove-gpu
    return 0
  fi
  if ! command -v nvidia-container-runtime-hook >/dev/null; then
    echo "▸ GPU：安裝 nvidia-container-toolkit（NVIDIA 官方 apt 套件庫，以其簽章金鑰驗證）"
    command -v apt-get >/dev/null || { echo "只支援以 apt 安裝 nvidia-container-toolkit。" >&2; exit 1; }
    curl -fsSL https://nvidia.github.io/libnvidia-container/gpgkey | gpg --dearmor --yes -o "$NV_KEYRING"
    curl -fsSL https://nvidia.github.io/libnvidia-container/stable/deb/nvidia-container-toolkit.list \
      | sed "s#deb https://#deb [signed-by=$NV_KEYRING] https://#g" > "$NV_LIST"
    apt-get update -q -o Dir::Etc::sourcelist="$NV_LIST" -o Dir::Etc::sourceparts=- -o APT::Get::List-Cleanup=0 >/dev/null
    DEBIAN_FRONTEND=noninteractive apt-get install -y -q nvidia-container-toolkit >/dev/null
  fi
  echo "▸ GPU：註冊 Docker runtime runsc-gpu（nvproxy，驅動 $driver）"
  daemon_json add-gpu
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
  gpu_setup
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
  echo "▸ 建置環境（網路 $BUILD_NET、BuildKit 映像）"
  docker network inspect "$BUILD_NET" >/dev/null 2>&1 \
    || docker network create --driver bridge --subnet "$BUILD_SUBNET" -o com.docker.network.bridge.name=cldbuild "$BUILD_NET" >/dev/null
  docker pull -q "$BUILDKIT_IMAGE" >/dev/null
  docker pull -q "$OSV_IMAGE" >/dev/null
  agent_install
  echo "✓ 完成。用 $PREFIX/bin/caloud-agent --self-check --state-dir $AGENT_STATE 驗證。"
}

# agent 執行檔與 systemd 服務；加入權杖只寫入 root 可讀的 env 檔，註冊後改用節點權杖
agent_install() {
  if [ -n "$AGENT_BIN" ]; then
    mkdir -p "$PREFIX/bin"
    install -m 755 "$AGENT_BIN" "$PREFIX/bin/caloud-agent"
  fi
  [ -n "$SERVER" ] || return 0
  [ -x "$PREFIX/bin/caloud-agent" ] || { echo "找不到 caloud-agent，請以 --agent 指定執行檔。" >&2; exit 1; }
  echo "▸ agent 服務（$SERVER）"
  mkdir -p "$(dirname "$AGENT_ENV")" "$AGENT_STATE"
  chmod 700 "$AGENT_STATE"
  ( umask 077
    { echo "CALOUD_AGENT_SERVER=$SERVER"; [ -n "$JOIN" ] && echo "CALOUD_AGENT_JOIN=$JOIN"; } > "$AGENT_ENV" )
  cat > "$AGENT_UNIT" <<UNIT
[Unit]
Description=Caloud Runner agent (ADR 0012, ADR 0018)
After=docker.service caloud-firewall.service network-online.target
Wants=docker.service network-online.target

[Service]
EnvironmentFile=$AGENT_ENV
ExecStart=$PREFIX/bin/caloud-agent --state-dir $AGENT_STATE
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
UNIT
  systemctl daemon-reload
  systemctl enable caloud-agent.service >/dev/null 2>&1
  systemctl restart caloud-agent.service
}

do_uninstall() {
  need_root
  systemctl disable --now caloud-agent.service >/dev/null 2>&1 || true
  rm -f "$AGENT_UNIT" "$AGENT_ENV"; rm -rf "$AGENT_STATE"
  docker network rm "$BUILD_NET" >/dev/null 2>&1 || true
  docker image rm "$BUILDKIT_IMAGE" "$OSV_IMAGE" >/dev/null 2>&1 || true
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
  echo "建置網路:      $(docker network inspect "$BUILD_NET" --format '{{index .Options "com.docker.network.bridge.name"}}' 2>/dev/null || echo 未建立)"
  echo "BuildKit:     $(docker image inspect --format '{{.Id}}' "$BUILDKIT_IMAGE" >/dev/null 2>&1 && echo 已拉取 || echo 未拉取)"
  echo "agent 服務:    $(systemctl is-active caloud-agent.service 2>/dev/null || echo 未安裝)"
  if command -v nvidia-smi >/dev/null; then
    echo "GPU:          $(nvidia-smi --query-gpu=name,memory.total --format=csv,noheader 2>/dev/null | paste -sd ';') 驅動 $(gpu_driver)"
    echo "nvproxy 支援:  $("$RUNSC" nvproxy list-supported-drivers 2>/dev/null | grep -qx "$(gpu_driver)" && echo 是 || echo 否)"
    echo "GPU hook:     $(command -v nvidia-container-runtime-hook >/dev/null && echo 已安裝 || echo 未安裝)"
  fi
}

AGENT_BIN="" SERVER="" JOIN=""
cmd="${1:-}"; [ $# -gt 0 ] && shift
while [ $# -gt 0 ]; do
  case "$1" in
    --agent) AGENT_BIN="${2:?}"; shift 2 ;;
    --server) SERVER="${2:?}"; shift 2 ;;
    --join) JOIN="${2:?}"; shift 2 ;;
    *) echo "未知參數：$1" >&2; exit 2 ;;
  esac
done
case "$SERVER" in ""|https://*) ;; *) echo "--server 需以 https:// 開頭。" >&2; exit 2 ;; esac

case "$cmd" in
  install) do_install ;;
  uninstall) do_uninstall ;;
  status) do_status ;;
  firewall-up) need_root; firewall_up ;;
  firewall-down) need_root; firewall_down ;;
  *) echo "用法：$0 install [--agent <執行檔>] [--server <網址> --join <權杖>]|status|uninstall" >&2; exit 2 ;;
esac
