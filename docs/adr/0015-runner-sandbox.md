# ADR 0015：Runner 環境沙箱（gVisor、固定網段名稱解析、防火牆、自我檢查）

- 狀態：Accepted
- 日期：2026-11-18
- 相關需求：FR-OPS-11、FR-OPS-12、FR-OPS-13

## 背景

平臺會執行用戶的程式碼，用戶容器必須跑在沙箱裡，環境之間、環境與主機、內網、雲端 metadata 都不能互通（設計稿 §13）。2026-11-18 在 cafeca-dev-004（Ubuntu 24.04、Docker 29.4、nftables 後端）實測得到：

- gVisor（runsc）以 daemon.json 的 `runtimes.runsc`（`path`）註冊後，`systemctl reload docker` 即生效，不會重啟既有容器；直接用 `io.containerd.runsc.v1` shim 會靜默失敗。新版 runsc 需要同目錄的 `gvisor-bin/`。
- gVisor 的網路堆疊不經過容器 netns 的 NAT，Docker 內建 DNS（127.0.0.11）無法使用；`--reproduce-nat`、`--reproduce-nftables` 都無效。
- 不同的 user-defined bridge 預設互不相通；但容器仍能連到主機（SSH、PostgreSQL）、區網與 docker0。
- 在 `DOCKER-USER` 與 `INPUT` 加規則後，同環境可通、可上網際網路；其他環境、主機、區網、metadata 都不通。runsc 與 runc 結果一致。

## 決策

- **runtime**：Runner 主機以 `scripts/runner-setup.sh` 安裝固定版本的 runsc（SHA256 驗證），並合併進 daemon.json 的 `runtimes.runsc`。預設 runtime 維持 runc，不影響主機上其他容器。
- **網路**：每個環境一個 bridge，名稱 `cld<索引十六進位>`。網段取 `10.240.0.0/12` 內第「索引」個 /24，由控制面以節點內最小未用索引配置。服務依序使用 `.10` 起的固定 IP。
- **名稱解析**：override 為每個服務加上 `extra_hosts`（全部服務名稱 → IP），並掛載 agent 產生的 `resolv.conf`（外部 DNS，預設 1.1.1.1、8.8.8.8）取代內建 DNS。
- **防火牆**：以介面萬用字元 `cld+` 一次設好，環境增減時不必改規則。
  - `CALOUD-ENV-FWD`（掛在 `DOCKER-USER`）：
    - 同介面（`-i cld+ -o cld+`）RETURN，跨 bridge 仍由 Docker 自己的隔離規則擋下；
    - ESTABLISHED、RELATED RETURN；
    - 目的地為 10/8、172.16/12、192.168/16、169.254/16、100.64/10 時 DROP；
    - TCP 25 DROP。
  - `CALOUD-ENV-IN`（掛在 `INPUT`）：ESTABLISHED、RELATED ACCEPT，其餘 `-i cld+` DROP。
- **自我檢查**：agent 啟動時檢查以下項目，結果寫入節點規格。只有通過的節點會被調度平臺環境（FR-OPS-12）。
  - runsc runtime；
  - 防火牆 chain；
  - 磁碟剩餘 10 GB 以上；
  - 用兩個探測環境實際驗證 gVisor、同環境名稱解析、跨環境不通、主機閘道被丟棄。
- **相依**：修訂 ADR 0012，`caloud-runner` 也依賴 `caloud-orchestrator`。
  - agent 用同一份平臺 compose 政策再驗一次控制面送來的 compose；
  - override 由 `caloud_orchestrator::sandbox` 產生，控制面與 agent 共用。
- **範圍**：M4 的環境只接受 image；建置（BuildKit rootless）在 M6 加入。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| **固定 IP＋extra_hosts＋自產 resolv.conf**（採用） | 不依賴 Docker 內建 DNS，runsc 與 runc 行為一致 | 環境內服務數上限 244；服務的 IP 由 Caloud 管理 |
| 每個環境一個 DNS sidecar | 支援動態名稱 | 多一個容器與攻擊面；sidecar 本身也要被隔離 |
| 改用 runsc `--network=host` | DNS 可用 | 完全失去網路隔離 |
| 每個環境動態加防火牆規則 | 規則可更精細 | 規則數量隨環境增長，清除失敗會殘留 |

## 影響

- [02 架構](../02-architecture.md)的相依圖新增 `caloud-runner ──► caloud-orchestrator`，並加上 `environments` 資料表。
- Runner 主機需要 Docker Compose 2.17 以上、python3、iptables。
- 環境容器看不到 Docker 內建 DNS；用戶程式若寫死 127.0.0.11 會失敗（極少見）。
