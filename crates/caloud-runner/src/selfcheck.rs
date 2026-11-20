//! 自我檢查（FR-OPS-12，ADR 0015）：通過才接平臺環境。
//!
//! 1. Docker 已註冊 `runsc` runtime
//! 2. 防火牆 chain `CALOUD-ENV-FWD`／`CALOUD-ENV-IN` 已建立並掛上
//! 3. 磁碟剩餘 ≥ 10 GB
//! 4. 兩個探測環境實際驗證：gVisor、同環境名稱解析、跨環境不通、主機閘道被丟棄
use crate::envs::EnvRunner;
use caloud_core::agent::{AgentMsg, EnvSpec};
use caloud_orchestrator::sandbox::{EnvNet, PROBE_INDEXES};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// 磁碟剩餘下限
pub const MIN_DISK_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const PROBE_IMAGE: &str = "busybox:1.36";
const PROBE_A: &str = "caloudselfchecka";
const PROBE_B: &str = "caloudselfcheckb";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelfCheck {
    /// Docker 已註冊 runsc
    pub gvisor: bool,
    /// 全部通過
    pub isolation: bool,
    /// 建置網路 `caloud-build` 與 BuildKit 映像就緒（ADR 0018）
    pub buildkit: bool,
    /// 未通過的原因
    pub reason: Option<String>,
}

impl SelfCheck {
    fn fail(gvisor: bool, reason: impl Into<String>) -> Self {
        Self {
            gvisor,
            isolation: false,
            buildkit: false,
            reason: Some(reason.into()),
        }
    }
}

async fn run(bin: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 防火牆規則是否齊全；回傳缺少的項目
async fn firewall_missing(iptables: &str) -> Vec<&'static str> {
    let mut missing = Vec::new();
    let fwd = run(iptables, &["-S", "CALOUD-ENV-FWD"]).await.unwrap_or_default();
    // iptables -S 的參數順序不固定（例：`-d 10.0.0.0/8 -i cld+ -j DROP`），逐條比對所有片段
    let has = |parts: &[&str]| fwd.lines().any(|l| parts.iter().all(|p| l.contains(p)));
    for (parts, label) in [
        (&["-i cld+", "-o cld+", "-j RETURN"][..], "同環境放行"),
        (&["-i cld+", "-d 169.254.0.0/16", "-j DROP"][..], "metadata"),
        (&["-i cld+", "-d 10.0.0.0/8", "-j DROP"][..], "10/8"),
        (&["-i cld+", "-d 172.16.0.0/12", "-j DROP"][..], "172.16/12"),
        (&["-i cld+", "-d 192.168.0.0/16", "-j DROP"][..], "192.168/16"),
        (&["-i cld+", "--dport 25", "-j DROP"][..], "TCP 25"),
    ] {
        if !has(parts) {
            missing.push(label);
        }
    }
    if !run(iptables, &["-S", "CALOUD-ENV-IN"])
        .await
        .unwrap_or_default()
        .contains("-i cld+ -j DROP")
    {
        missing.push("主機本身");
    }
    if run(iptables, &["-C", "DOCKER-USER", "-j", "CALOUD-ENV-FWD"]).await.is_none() {
        missing.push("DOCKER-USER 掛載");
    }
    if run(iptables, &["-C", "INPUT", "-j", "CALOUD-ENV-IN"]).await.is_none() {
        missing.push("INPUT 掛載");
    }
    missing
}

/// 執行自我檢查；`disk_free` 為 Docker 資料目錄的剩餘空間
pub async fn check(env: &EnvRunner, iptables: &str, disk_free: u64) -> SelfCheck {
    let runtimes = env
        .docker(&["info", "--format", "{{json .Runtimes}}"], Duration::from_secs(30))
        .await
        .unwrap_or_default();
    let gvisor = runtimes.contains("\"runsc\"");
    if !gvisor {
        return SelfCheck::fail(false, "Docker 沒有 runsc runtime，請先執行 scripts/runner-setup.sh install。");
    }
    let missing = firewall_missing(iptables).await;
    if !missing.is_empty() {
        return SelfCheck::fail(
            true,
            format!(
                "環境防火牆不完整（{}），請執行 scripts/runner-setup.sh install。",
                missing.join("、")
            ),
        );
    }
    if disk_free < MIN_DISK_BYTES {
        return SelfCheck::fail(true, format!("磁碟剩餘 {} GB，低於 10 GB。", disk_free / 1024 / 1024 / 1024));
    }
    let result = probe(env).await;
    for id in [PROBE_A, PROBE_B] {
        let _ = env.destroy(id).await;
    }
    match result {
        Ok(()) => SelfCheck {
            gvisor: true,
            isolation: true,
            buildkit: buildkit_ready(env).await,
            reason: None,
        },
        Err(e) => SelfCheck::fail(true, format!("探測環境未通過：{e}")),
    }
}

/// 建置網路的介面名稱須符合 `cld+`（沿用環境防火牆），BuildKit 映像已拉取
async fn buildkit_ready(env: &EnvRunner) -> bool {
    let bridge = env
        .docker(
            &[
                "network",
                "inspect",
                crate::build::BUILD_NETWORK,
                "--format",
                "{{index .Options \"com.docker.network.bridge.name\"}}",
            ],
            Duration::from_secs(30),
        )
        .await
        .unwrap_or_default();
    bridge.trim().starts_with("cld")
        && env
            .docker(
                &["image", "inspect", "--format", "{{.Id}}", &env.buildkit_image],
                Duration::from_secs(30),
            )
            .await
            .is_ok()
}

async fn probe(env: &EnvRunner) -> Result<(), String> {
    let web =
        format!("  web:\n    image: {PROBE_IMAGE}\n    command: [\"sh\", \"-c\", \"echo ok > /tmp/i && httpd -f -p 8000 -h /tmp\"]\n");
    let a = EnvSpec {
        id: PROBE_A.into(),
        kind: "verify".into(),
        compose: format!("services:\n{web}  probe:\n    image: {PROBE_IMAGE}\n    command: [\"sleep\", \"600\"]\n"),
        subnet_index: PROBE_INDEXES[0],
        public_service: "web".into(),
        source: None,
        vars: vec![],
    };
    let b = EnvSpec {
        id: PROBE_B.into(),
        compose: format!("services:\n{web}"),
        subnet_index: PROBE_INDEXES[1],
        ..a.clone()
    };
    for spec in [&a, &b] {
        if let AgentMsg::EnvStatus { status, error, .. } = env.deploy_unchecked(spec).await {
            if status != "running" {
                return Err(format!("無法啟動探測環境：{}", error.unwrap_or_default()));
            }
        }
    }
    let net_a = EnvNet::new(PROBE_INDEXES[0]).ok_or("索引錯誤")?;
    let net_b = EnvNet::new(PROBE_INDEXES[1]).ok_or("索引錯誤")?;
    let other = net_b.service_ip(0).ok_or("索引錯誤")?;
    let gw = net_a.gateway();
    let script = format!(
        "case $(uname -r) in *gvisor*) echo gvisor=1;; *) echo gvisor=0;; esac; \
         wget -q -T 5 -O /dev/null http://web:8000/i && echo same=1 || echo same=0; \
         nc -z -w 3 {other} 8000 && echo cross=1 || echo cross=0; \
         s=$(date +%s); nc -z -w 3 {gw} 1; e=$(date +%s); [ $((e-s)) -ge 2 ] && echo gw_dropped=1 || echo gw_dropped=0"
    );
    let out = env
        .compose(PROBE_A, &["exec", "-T", "probe", "sh", "-c", &script], Duration::from_secs(60))
        .await
        .map_err(|e| e.to_string())?;
    let got = |k: &str| out.lines().any(|l| l.trim() == format!("{k}=1"));
    let mut failed = Vec::new();
    if !got("gvisor") {
        failed.push("容器不是在 gVisor 中執行");
    }
    if !got("same") {
        failed.push("同環境無法以名稱連線");
    }
    if got("cross") {
        failed.push("可以連到其他環境");
    }
    if !got("gw_dropped") {
        failed.push("可以連到 Runner 主機");
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("、"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn script(dir: &std::path::Path, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.display().to_string()
    }

    // FR-OPS-12：以替身 docker／iptables 驗證判斷邏輯
    #[cfg(unix)]
    #[tokio::test]
    async fn self_check_requires_runtime_firewall_disk_and_probe() {
        let base = std::env::temp_dir().join(format!("caloud-selfcheck-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let fw_ok = "case \"$*\" in '-S CALOUD-ENV-FWD') printf -- '-A CALOUD-ENV-FWD -i cld+ -o cld+ -j RETURN\\n-A CALOUD-ENV-FWD -d 10.0.0.0/8 -i cld+ -j DROP\\n-A CALOUD-ENV-FWD -d 172.16.0.0/12 -i cld+ -j DROP\\n-A CALOUD-ENV-FWD -d 192.168.0.0/16 -i cld+ -j DROP\\n-A CALOUD-ENV-FWD -d 169.254.0.0/16 -i cld+ -j DROP\\n-A CALOUD-ENV-FWD -i cld+ -p tcp -m tcp --dport 25 -j DROP\\n';; '-S CALOUD-ENV-IN') echo '-A CALOUD-ENV-IN -i cld+ -j DROP';; esac; exit 0";
        let iptables_ok = script(&base, "ipt-ok.sh", fw_ok);
        let iptables_none = script(&base, "ipt-none.sh", "exit 1");
        let docker = |name: &str, runtimes: &str, probe: &str| {
            let body = format!(
                "echo \"$*\" >> {log}\ncase \"$*\" in 'info '*) echo '{runtimes}';; *' exec '*) printf '{probe}';; 'network inspect caloud-build'*) echo cldbuild;; esac; exit 0",
                log = base.join("docker.log").display()
            );
            script(&base, name, &body)
        };
        let good_probe = "gvisor=1\\nsame=1\\ncross=0\\ngw_dropped=1\\n";
        let ten_gb = MIN_DISK_BYTES;

        let r = EnvRunner::new(docker("d1.sh", "{\"runc\":{}}", good_probe), base.join("s1"), vec![]);
        let c = check(&r, &iptables_ok, ten_gb).await;
        assert!(!c.gvisor && !c.isolation && c.reason.unwrap().contains("runsc"));

        let r = EnvRunner::new(docker("d2.sh", "{\"runc\":{},\"runsc\":{}}", good_probe), base.join("s2"), vec![]);
        let c = check(&r, &iptables_none, ten_gb).await;
        assert!(c.gvisor && !c.isolation && c.reason.unwrap().contains("防火牆"));
        let c = check(&r, &iptables_ok, ten_gb - 1).await;
        assert!(c.reason.unwrap().contains("磁碟"));
        let c = check(&r, &iptables_ok, ten_gb).await;
        assert_eq!(
            c,
            SelfCheck {
                gvisor: true,
                isolation: true,
                buildkit: true,
                reason: None
            }
        );
        let log = std::fs::read_to_string(base.join("docker.log")).unwrap();
        assert!(log.contains("caloud-env-caloudselfchecka") && log.contains("caloud-env-caloudselfcheckb"));
        assert!(log.contains("down -v"), "探測環境會被清除");
        assert!(!base.join("s2/envs/caloudselfchecka").exists());

        let r = EnvRunner::new(
            docker("d3.sh", "{\"runsc\":{}}", "gvisor=0\\nsame=1\\ncross=1\\ngw_dropped=0\\n"),
            base.join("s3"),
            vec![],
        );
        let reason = check(&r, &iptables_ok, ten_gb).await.reason.unwrap();
        assert!(
            reason.contains("gVisor") && reason.contains("其他環境") && reason.contains("Runner 主機"),
            "{reason}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
