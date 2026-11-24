//! 驗證（M6，[ADR 0020](../../../docs/adr/0020-verification.md)）：agent 負責的八個階段，逐步回報。
//!
//! `fetch` → `build` → `static`（秘密外洩、漏洞、lint）→ `start` → `test` → `smoke` → `logs` → `destroy`；
//! `analyze` 與 `report` 由控制面處理。前面的階段失敗時，需要執行環境的階段標為 skipped；
//! `destroy` 一定執行（失敗且要求保留時除外）。取消時終止子程序、清除建置與測試容器並銷毀環境。
use crate::build::{self, mask, Log};
use crate::envs::{short, EnvRunner};
use caloud_core::agent::{valid_env_id, AgentMsg, EnvService, Issue, VerifyCmd, VerifySpec};
use caloud_orchestrator::sandbox;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

/// osv-scanner v2.3.0（以 digest 固定）
pub const OSV_IMAGE: &str = "ghcr.io/google/osv-scanner@sha256:6421cdd773a54fad5fd991895b4f3840beba55a2f30a8182185919e13979a2c8";
/// 測試、lint 的逾時
pub const TEST_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// 漏洞掃描的逾時
const SCAN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// log 關鍵字（區分大小寫的子字串）
const LOG_PATTERNS: &[&str] = &[
    "panic",
    "FATAL",
    "Unhandled",
    "UnhandledPromiseRejection",
    "Traceback (most recent call last)",
    "ECONNREFUSED",
    "Segmentation fault",
    "OutOfMemory",
    "out of memory",
];
const MAX_LOG_ISSUES: usize = 20;

/// 回報工具
struct Report<'a> {
    tx: &'a UnboundedSender<AgentMsg>,
    id: &'a str,
    secrets: Vec<String>,
    /// 目前為止是否有 Blocker／High
    bad: bool,
    /// 有 Blocker／High 的階段
    bad_stages: std::collections::HashSet<String>,
}

impl Report<'_> {
    fn stage(&self, stage: &str, status: &str) {
        let _ = self.tx.send(AgentMsg::VerifyStage {
            id: self.id.into(),
            stage: stage.into(),
            status: status.into(),
        });
    }

    fn issue(&mut self, mut i: Issue) {
        if matches!(i.severity.as_str(), "blocker" | "high") {
            self.bad = true;
            self.bad_stages.insert(i.stage.clone());
        }
        i.title = mask(&i.title, &self.secrets).chars().take(200).collect();
        i.detail = mask(&i.detail, &self.secrets).chars().take(1000).collect();
        let _ = self.tx.send(AgentMsg::VerifyIssue {
            id: self.id.into(),
            issue: i,
        });
    }

    /// 依這個階段是否有 Blocker／High 回報結果
    fn finish(&self, stage: &str) {
        self.stage(stage, if self.bad_stages.contains(stage) { "failed" } else { "passed" });
    }

    fn simple(&mut self, severity: &str, stage: &str, rule: &str, title: impl Into<String>, detail: impl Into<String>) {
        self.issue(Issue {
            severity: severity.into(),
            stage: stage.into(),
            rule: rule.into(),
            title: title.into(),
            detail: detail.into(),
            file: None,
            line: None,
        });
    }
}

/// CVSS 分數 → 等級（ADR 0020）
pub fn cvss_severity(score: &str) -> &'static str {
    match score.trim().parse::<f32>() {
        Ok(s) if s >= 9.0 => "high",
        Ok(s) if s >= 7.0 => "medium",
        _ => "low",
    }
}

/// osv-scanner `--format json` → 問題
pub fn parse_osv(json: &str) -> Vec<Issue> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return vec![];
    };
    let mut out = Vec::new();
    for r in v["results"].as_array().into_iter().flatten() {
        let path = r["source"]["path"].as_str().unwrap_or_default();
        let file = path.strip_prefix("/src/").unwrap_or(path).to_string();
        for p in r["packages"].as_array().into_iter().flatten() {
            let name = p["package"]["name"].as_str().unwrap_or("?");
            let version = p["package"]["version"].as_str().unwrap_or("?");
            for g in p["groups"].as_array().into_iter().flatten() {
                let ids: Vec<&str> = g["ids"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).collect();
                let Some(id) = ids.first() else { continue };
                let summary = p["vulnerabilities"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|x| x["id"].as_str() == Some(id))
                    .and_then(|x| x["summary"].as_str())
                    .unwrap_or_default();
                let aliases: Vec<&str> = g["aliases"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).collect();
                let score = g["max_severity"].as_str().unwrap_or_default();
                out.push(Issue {
                    severity: cvss_severity(score).into(),
                    stage: "static".into(),
                    rule: format!("vuln/{id}"),
                    title: format!("{name}@{version} 有已知漏洞 {id}"),
                    detail: format!(
                        "{summary}{}{}",
                        if score.is_empty() {
                            String::new()
                        } else {
                            format!("（CVSS {score}）")
                        },
                        if aliases.is_empty() {
                            String::new()
                        } else {
                            format!(" 別名：{}", aliases.join("、"))
                        }
                    ),
                    file: (!file.is_empty()).then_some(file.clone()),
                    line: None,
                });
            }
        }
    }
    out
}

/// 把 log 行正規化（去掉時間、數字），相同的行只算一次
fn normalize(line: &str) -> String {
    line.chars()
        .map(|c| if c.is_ascii_digit() { '0' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `compose logs` 的輸出 → 問題（ADR 0020 `logs` 階段）
pub fn scan_logs(text: &str) -> Vec<Issue> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for l in text.lines() {
        let Some(p) = LOG_PATTERNS.iter().find(|p| l.contains(*p)) else {
            continue;
        };
        let (svc, msg) = match l.split_once(" | ") {
            Some((s, m)) => (s.trim().rsplit_once('-').map(|(a, _)| a).unwrap_or(s.trim()).to_string(), m.trim()),
            None => (String::new(), l.trim()),
        };
        if !seen.insert(normalize(msg)) {
            continue;
        }
        out.push(Issue {
            severity: "medium".into(),
            stage: "logs".into(),
            rule: format!("logs/{}", p.to_ascii_lowercase().replace(' ', "-")),
            title: if svc.is_empty() {
                format!("log 出現 `{p}`")
            } else {
                format!("`{svc}` 的 log 出現 `{p}`")
            },
            detail: msg.chars().take(300).collect(),
            file: None,
            line: None,
        });
        if out.len() >= MAX_LOG_ISSUES {
            break;
        }
    }
    out
}

/// HTTP/1.1 GET，回傳狀態碼
async fn http_status(target: &str, path: &str) -> Option<u16> {
    let job = async {
        let mut c = tokio::net::TcpStream::connect(target).await.ok()?;
        c.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nUser-Agent: Caloud-Verify\r\nConnection: close\r\n\r\n").as_bytes())
            .await
            .ok()?;
        let mut buf = [0u8; 32];
        let mut n = 0;
        while n < 12 {
            let k = c.read(&mut buf[n..]).await.ok()?;
            if k == 0 {
                break;
            }
            n += k;
        }
        let head = std::str::from_utf8(&buf[..n]).ok()?;
        head.strip_prefix("HTTP/")?.split_whitespace().nth(1)?.parse().ok()
    };
    tokio::time::timeout(Duration::from_secs(10), job).await.ok().flatten()
}

impl EnvRunner {
    /// 取消進行中的驗證
    pub fn cancel_verify(&self, id: &str) {
        if let Some(tx) = self.runs.lock().unwrap_or_else(|e| e.into_inner()).get(id) {
            let _ = tx.send(true);
        }
    }

    /// 執行驗證並逐步回報；一定以 `VerifyDone` 結束
    pub async fn verify(&self, spec: &VerifySpec, tx: &UnboundedSender<AgentMsg>) {
        let env_id = spec.env.id.clone();
        let done = |error: Option<String>, kept: bool| {
            // ADR 0021：建置與沙箱的用量在結束前回報
            self.send_usage(Some(tx), "verify", &env_id, &spec.id);
            let _ = tx.send(AgentMsg::VerifyDone {
                id: spec.id.clone(),
                error,
                kept,
            });
        };
        if !valid_env_id(&env_id) || spec.env.subnet_index > sandbox::MAX_ALLOC_INDEX || spec.env.source.is_none() {
            return done(Some("驗證規格錯誤。".into()), false);
        }
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
        self.runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(spec.id.clone(), cancel_tx);
        let mut rep = Report {
            tx,
            id: &spec.id,
            secrets: spec.env.vars.iter().map(|v| v.value.clone()).collect(),
            bad: false,
            bad_stages: Default::default(),
        };
        let result = tokio::select! {
            r = self.pipeline(spec, &mut rep) => Some(r),
            _ = cancel_rx.wait_for(|c| *c) => None,
        };
        self.runs.lock().unwrap_or_else(|e| e.into_inner()).remove(&spec.id);
        match result {
            None => {
                // 取消：子程序已隨 future 結束；清除以名稱標記的容器後銷毀
                self.remove_named(&env_id).await;
                let _ = self.destroy(&env_id).await;
                rep.stage("destroy", "passed");
                done(Some("已取消".into()), false);
            }
            Some(err) => {
                let failed = rep.bad || err.is_some();
                if failed && spec.keep_on_failure {
                    rep.stage("destroy", "skipped");
                    let _ = tokio::fs::remove_dir_all(self.builds_dir(&env_id)).await;
                    return done(err, true);
                }
                rep.stage("destroy", "running");
                self.remove_named(&env_id).await;
                match self.destroy(&env_id).await {
                    AgentMsg::EnvStatus { status, error, .. } if status != "destroyed" => {
                        rep.stage("destroy", "failed");
                        rep.simple("low", "destroy", "destroy/failed", "驗證環境銷毀失敗", error.unwrap_or_default());
                    }
                    _ => rep.stage("destroy", "passed"),
                }
                done(err, false);
            }
        }
    }

    /// 建置與測試容器（逾時或取消時 CLI 已終止，容器要另外移除）
    async fn remove_named(&self, env_id: &str) {
        for prefix in [
            format!("caloud-build-{env_id}-"),
            format!("caloud-test-{env_id}-"),
            format!("caloud-osv-{env_id}"),
        ] {
            if let Ok(ids) = self
                .docker(&["ps", "-aq", "--filter", &format!("name={prefix}")], Duration::from_secs(30))
                .await
            {
                let mut args = vec!["rm", "-f"];
                args.extend(ids.split_whitespace());
                if args.len() > 2 {
                    let _ = self.docker(&args, Duration::from_secs(60)).await;
                }
            }
        }
    }

    /// 回傳 Err 表示流程本身出錯（不是問題）
    async fn pipeline(&self, spec: &VerifySpec, rep: &mut Report<'_>) -> Option<String> {
        let env = &spec.env;
        let src = env.source.as_ref()?;
        let tx = rep.tx;
        let id = spec.id.as_str();
        let send_stage = |s: &str| {
            let (stage, status) = match s {
                "fetch" => ("fetch", "running"),
                "build" => {
                    let _ = tx.send(AgentMsg::VerifyStage {
                        id: id.into(),
                        stage: "fetch".into(),
                        status: "passed".into(),
                    });
                    ("build", "running")
                }
                _ => return,
            };
            let _ = tx.send(AgentMsg::VerifyStage {
                id: id.into(),
                stage: stage.into(),
                status: status.into(),
            });
        };
        let work = self.builds_dir(&env.id);
        let repo = work.join("src");
        let built = {
            let _guard_lock = self.lock(&env.id);
            let guard = _guard_lock.lock().await;
            let r = self.build_source_with(env, src, Some(tx), &send_stage, Some(id)).await;
            drop(guard);
            r
        };
        let have_src = tokio::fs::try_exists(&repo).await.unwrap_or(false);
        let compose = match built {
            Ok((compose, services)) => {
                rep.stage("build", "passed");
                Some((compose, services))
            }
            Err((st, e)) => {
                rep.stage(st, "failed");
                let title = if st == "fetch" { "無法取得原始碼" } else { "建置失敗" };
                rep.simple("blocker", st, &format!("{st}/failed"), title, short(&e.to_string()));
                if st == "fetch" {
                    for s in ["build", "static", "start", "test", "smoke", "logs"] {
                        rep.stage(s, "skipped");
                    }
                    return None;
                }
                None
            }
        };

        // static：秘密外洩、漏洞、lint（只需要原始碼）
        if have_src {
            rep.stage("static", "running");
            let r = repo.clone();
            let found = tokio::task::spawn_blocking(move || crate::secrets::scan(&r))
                .await
                .unwrap_or_default();
            let mut log = Log::new(Some(tx), id, "static").for_verify(Some(id));
            log.line(&format!("秘密外洩掃描：{} 項", found.len()));
            for i in found {
                rep.issue(i);
            }
            self.osv(spec, &repo, rep, &mut log).await;
            if let Some(lint) = &spec.lint {
                match self.sandbox(spec, "lint", lint, &repo, "static", tx).await {
                    Ok(()) => log.line("lint 通過"),
                    Err(e) => rep.simple("medium", "static", "lint/failed", "lint 未通過", e),
                }
            }
            rep.finish("static");
        } else {
            rep.stage("static", "skipped");
        }

        // start
        let mut services: Vec<EnvService> = vec![];
        let started = match &compose {
            Some((yaml, built)) => {
                rep.stage("start", "running");
                let names: Vec<String> = env.vars.iter().filter(|v| v.user).map(|v| v.name.clone()).collect();
                let inject: Vec<(String, Vec<String>)> = built.iter().map(|s| (s.clone(), names.clone())).collect();
                match self.try_deploy(env, yaml, &inject).await {
                    Ok(s) => {
                        services = s;
                        // ADR 0021：發布前任務（migration、seed），失敗為 Blocker
                        let mut log = Log::new(Some(tx), id, "start").for_verify(Some(id)).masking(&rep.secrets);
                        match self.run_release(env, false, &mut log).await {
                            Ok(()) => {
                                rep.stage("start", "passed");
                                true
                            }
                            Err(e) => {
                                rep.stage("start", "failed");
                                rep.simple("blocker", "start", "release/failed", "發布前任務失敗", short(&e.to_string()));
                                false
                            }
                        }
                    }
                    Err(e) => {
                        rep.stage("start", "failed");
                        rep.simple("blocker", "start", "start/failed", "服務無法啟動", short(&e.to_string()));
                        false
                    }
                }
            }
            None => {
                rep.stage("start", "skipped");
                false
            }
        };

        // test（只需要原始碼；測試失敗不略過 smoke 與 logs）
        match (&spec.test, have_src) {
            (Some(t), true) => {
                rep.stage("test", "running");
                match self.sandbox(spec, "test", t, &repo, "test", tx).await {
                    Ok(()) => rep.stage("test", "passed"),
                    Err(e) => {
                        rep.stage("test", "failed");
                        rep.simple("high", "test", "test/failed", format!("專案測試未通過（{}）", t.command), e);
                    }
                }
            }
            (None, true) => {
                rep.stage("test", "skipped");
                rep.simple(
                    "low",
                    "test",
                    "test/none",
                    "沒有偵測到專案測試",
                    "可在 package.json 加上 test 指令，或以 .caloud/verify.yml 指定。",
                );
            }
            _ => rep.stage("test", "skipped"),
        }

        if !started {
            rep.stage("smoke", "skipped");
            rep.stage("logs", "skipped");
            return None;
        }

        // smoke
        rep.stage("smoke", "running");
        let ip = services.iter().find(|s| s.name == env.public_service).map(|s| s.ip.clone());
        let target = self
            .smoke_target
            .clone()
            .or_else(|| ip.map(|ip| format!("{ip}:{}", spec.public_port)));
        let mut log = Log::new(Some(tx), id, "smoke").for_verify(Some(id));
        match target {
            None => rep.simple("blocker", "smoke", "smoke/no-service", "找不到對外服務", env.public_service.clone()),
            Some(target) => {
                let paths = if spec.smoke.is_empty() {
                    vec!["/".to_string()]
                } else {
                    spec.smoke.clone()
                };
                for (i, p) in paths.iter().enumerate() {
                    // 第一個路徑最多等 30 秒（服務可能仍在初始化）
                    let mut code = http_status(&target, p).await;
                    if i == 0 {
                        for _ in 0..5 {
                            if code.is_some() {
                                break;
                            }
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            code = http_status(&target, p).await;
                        }
                    }
                    match code {
                        None => rep.simple(
                            "blocker",
                            "smoke",
                            "smoke/no-response",
                            format!("`{p}` 沒有回應"),
                            format!("服務 `{}` 沒有回應 HTTP", env.public_service),
                        ),
                        Some(c) if c >= 500 => rep.simple("high", "smoke", "smoke/5xx", format!("`{p}` 回應 {c}"), String::new()),
                        Some(c) => log.line(&format!("GET {p} → {c}")),
                    }
                }
            }
        }
        rep.finish("smoke");

        // logs
        rep.stage("logs", "running");
        self.check_containers(env, rep).await;
        if let Ok(out) = self
            .compose(&env.id, &["logs", "--no-color", "--tail", "2000"], Duration::from_secs(60))
            .await
        {
            for i in scan_logs(&out) {
                rep.issue(i);
            }
        }
        rep.finish("logs");
        None
    }

    /// 容器重啟或以非 0 結束
    async fn check_containers(&self, env: &caloud_core::agent::EnvSpec, rep: &mut Report<'_>) {
        let Ok(ids) = self.compose(&env.id, &["ps", "-aq"], Duration::from_secs(30)).await else {
            return;
        };
        for cid in ids.split_whitespace() {
            let Ok(out) = self
                .docker(
                    &[
                        "inspect",
                        "-f",
                        "{{.Name}} {{.RestartCount}} {{.State.ExitCode}} {{.State.Status}}",
                        cid,
                    ],
                    Duration::from_secs(30),
                )
                .await
            else {
                continue;
            };
            let parts: Vec<&str> = out.split_whitespace().collect();
            let [name, restarts, code, status] = parts[..] else { continue };
            let name = name.trim_start_matches('/');
            if restarts.parse::<u32>().unwrap_or(0) > 0 {
                rep.simple(
                    "high",
                    "logs",
                    "logs/restart",
                    format!("`{name}` 重啟了 {restarts} 次"),
                    String::new(),
                );
            }
            if status == "exited" && code != "0" {
                rep.simple("high", "logs", "logs/exit", format!("`{name}` 以 {code} 結束"), String::new());
            }
        }
    }

    /// 漏洞掃描（osv-scanner，gVisor）
    async fn osv(&self, spec: &VerifySpec, repo: &Path, rep: &mut Report<'_>, log: &mut Log<'_>) {
        let work = self.builds_dir(&spec.env.id);
        let resolv = work.join("resolv.conf");
        let _ = tokio::fs::write(&resolv, sandbox::resolv_conf(&self.dns)).await;
        let name = format!("caloud-osv-{}", spec.env.id);
        let limits = spec.env.build.unwrap_or_default();
        let started = std::time::Instant::now();
        let args = [
            "run".to_string(),
            "--rm".into(),
            "--name".into(),
            name.clone(),
            "--runtime".into(),
            "runsc".into(),
            "--network".into(),
            build::BUILD_NETWORK.into(),
            "--cpus".into(),
            limits.cpus.min(2).to_string(),
            "--memory".into(),
            format!("{}m", limits.memory_mb.min(4096)),
            "-v".into(),
            format!("{}:/etc/resolv.conf:ro", resolv.display()),
            "-v".into(),
            format!("{}:/src:ro", repo.display()),
            self.osv_image.clone(),
            "scan".into(),
            "source".into(),
            "-r".into(),
            "/src".into(),
            "--format".into(),
            "json".into(),
        ];
        let mut c = Command::new(&self.docker_bin);
        c.args(&args).stdin(Stdio::null()).kill_on_drop(true);
        let out = tokio::time::timeout(SCAN_TIMEOUT, c.output()).await;
        self.add_usage(&spec.env.id, started.elapsed());
        match out {
            Ok(Ok(out)) => {
                // 0：沒有漏洞；1：有漏洞；128：沒有可掃描的套件
                let code = out.status.code().unwrap_or(-1);
                let found = parse_osv(&String::from_utf8_lossy(&out.stdout));
                if code == 0 || code == 1 || code == 128 {
                    log.line(&format!("相依套件漏洞掃描：{} 項", found.len()));
                } else {
                    log.line(&format!("漏洞掃描失敗（{code}）：{}", short(&String::from_utf8_lossy(&out.stderr))));
                    rep.simple(
                        "low",
                        "static",
                        "vuln/scan-failed",
                        "相依套件漏洞掃描失敗",
                        short(&String::from_utf8_lossy(&out.stderr)),
                    );
                }
                for i in found {
                    rep.issue(i);
                }
            }
            _ => {
                let _ = self.docker(&["rm", "-f", &name], Duration::from_secs(30)).await;
                rep.simple("low", "static", "vuln/scan-failed", "相依套件漏洞掃描逾時", String::new());
            }
        }
    }

    /// 在 gVisor 測試沙箱中執行指令；失敗時回傳最後幾行輸出
    async fn sandbox(
        &self,
        spec: &VerifySpec,
        kind: &str,
        cmd: &VerifyCmd,
        repo: &Path,
        stage: &'static str,
        tx: &UnboundedSender<AgentMsg>,
    ) -> Result<(), String> {
        let env_id = &spec.env.id;
        let work = self.builds_dir(env_id);
        // 可寫的原始碼副本，避免影響下一個沙箱
        let copy = work.join(format!("{kind}-src"));
        let _ = tokio::fs::remove_dir_all(&copy).await;
        let cp = Command::new("cp")
            .arg("-a")
            .arg(repo)
            .arg(&copy)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| e.to_string())?;
        if !cp.status.success() {
            return Err("無法準備原始碼副本".into());
        }
        let resolv = work.join("resolv.conf");
        let _ = tokio::fs::write(&resolv, sandbox::resolv_conf(&self.dns)).await;
        let name = format!("caloud-test-{env_id}-{kind}");
        let limits = spec.env.build.unwrap_or_default();
        let (cpus, mem) = (limits.cpus.to_string(), format!("{}m", limits.memory_mb));
        let script = if cmd.setup.trim().is_empty() {
            cmd.command.clone()
        } else {
            format!("{} && {}", cmd.setup, cmd.command)
        };
        // ADR 0022：專案有 GPU 服務時，測試與 lint 沙箱也可用 GPU
        let gpu = sandbox::declares_gpu(&spec.env.compose);
        let runtime = if gpu { sandbox::RUNTIME_GPU } else { sandbox::RUNTIME };
        let mut c = Command::new(&self.docker_bin);
        c.args(["run", "--rm", "--name", &name, "--runtime", runtime]);
        if gpu {
            c.args(["--gpus", "all"]);
        }
        c.args([
            "--network",
            build::BUILD_NETWORK,
            "--cpus",
            &cpus,
            "--memory",
            &mem,
            "--pids-limit",
            "4096",
            "-e",
            "CI=true",
            "-v",
            &format!("{}:/etc/resolv.conf:ro", resolv.display()),
            "-v",
            &format!("{}:/src", copy.display()),
            "-w",
            "/src",
            &cmd.image,
            "sh",
            "-c",
            &script,
        ]);
        let secrets: Vec<String> = spec.env.vars.iter().map(|v| v.value.clone()).collect();
        let mut log = Log::new(Some(tx), &spec.id, stage).for_verify(Some(&spec.id)).masking(&secrets);
        log.line(&format!("$ {script}"));
        let started = std::time::Instant::now();
        let res = tokio::time::timeout(TEST_TIMEOUT, build::run_logged(c, &mut log)).await;
        self.add_usage(env_id, started.elapsed());
        let _ = tokio::fs::remove_dir_all(&copy).await;
        match res {
            Err(_) => {
                let _ = self.docker(&["rm", "-f", &name], Duration::from_secs(30)).await;
                Err(format!("超過 {} 分鐘，已中止。", TEST_TIMEOUT.as_secs() / 60))
            }
            Ok(Err(e)) => Err(e.to_string()),
            Ok(Ok(())) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caloud_core::agent::{EnvSource, EnvSpec, EnvValue};

    const OSV_JSON: &str = r#"{"results":[{"source":{"path":"/src/package-lock.json","type":"lockfile"},"packages":[{"package":{"name":"lodash","version":"4.17.20","ecosystem":"npm"},"vulnerabilities":[{"id":"GHSA-35jh-r3h4-6jhm","summary":"Command Injection in lodash"}],"groups":[{"ids":["GHSA-35jh-r3h4-6jhm"],"aliases":["CVE-2021-23337"],"max_severity":"7.2"}]},{"package":{"name":"x","version":"1"},"vulnerabilities":[],"groups":[{"ids":["GHSA-crit"],"aliases":[],"max_severity":"9.8"},{"ids":["GHSA-low"],"aliases":[],"max_severity":""}]}]}]}"#;

    #[test]
    fn parses_osv_and_logs() {
        let v = parse_osv(OSV_JSON);
        let s: Vec<(&str, &str)> = v.iter().map(|i| (i.rule.as_str(), i.severity.as_str())).collect();
        assert_eq!(
            s,
            [
                ("vuln/GHSA-35jh-r3h4-6jhm", "medium"),
                ("vuln/GHSA-crit", "high"),
                ("vuln/GHSA-low", "low")
            ]
        );
        assert_eq!(v[0].file.as_deref(), Some("package-lock.json"));
        assert!(v[0].detail.contains("CVE-2021-23337") && v[0].detail.contains("CVSS 7.2"));
        assert!(parse_osv("not json").is_empty());

        let logs = "web-1  | ready\nweb-1  | Error: connect ECONNREFUSED 10.0.0.1:5432\nweb-1  | Error: connect ECONNREFUSED 10.0.0.2:5432\napi-2  | thread 'main' panicked at x\n";
        let i = scan_logs(logs);
        assert_eq!(i.len(), 2, "數字不同的相同訊息只算一次：{i:?}");
        assert_eq!(i[0].title, "`web` 的 log 出現 `ECONNREFUSED`");
        assert_eq!(i[1].rule, "logs/panic");
    }

    /// 替身 docker：依參數決定行為；`TEST_FAIL` 檔存在時測試失敗，`BUILD_FAIL` 時建置失敗
    #[cfg(unix)]
    fn fake(base: &Path) -> (EnvRunner, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join("app")).unwrap();
        std::fs::write(repo.join("app/Dockerfile"), "FROM alpine\n").unwrap();
        std::fs::write(repo.join("app/config.js"), format!("const k = 'AKIA{}';\n", "ABCDEFGHIJKLMNOP")).unwrap();
        let log = base.join("docker.log");
        let docker = base.join("docker.sh");
        std::fs::write(
            &docker,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{log}'\n\
                 case \"$*\" in\n\
                 *buildctl-daemonless*) [ -f '{b}/BUILD_FAIL' ] && {{ echo 'error: failed to solve' >&2; exit 1; }}; echo '#1 done';;\n\
                 *osv-scanner*) printf '%s' '{osv}'; exit 1;;\n\
                 *caloud-test-*-test*) [ -f '{b}/SLOW' ] && sleep 30; [ -f '{b}/TEST_FAIL' ] && {{ echo '1 failing'; exit 1; }}; echo 'ok 3 tests';;\n\
                 *caloud-test-*-lint*) echo 'lint ok';;\n\
                 *' ps -aq'*) echo c1;;\n\
                 'inspect '*) echo '/web-1 2 0 running';;\n\
                 *' logs '*) echo 'web-1  | FATAL boom';;\n\
                 esac\nexit 0\n",
                log = log.display(),
                b = base.display(),
                osv = OSV_JSON
            ),
        )
        .unwrap();
        std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
        let git = base.join("git.sh");
        std::fs::write(
            &git,
            format!(
                "#!/bin/sh\ncase \"$*\" in *' checkout '*) cp -R '{repo}/.' \"$2\";; *' rev-parse '*) echo 98b705d0123456789abcdef0123456789abcdef0;; esac\nexit 0\n",
                repo = repo.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut r = EnvRunner::new(docker.display().to_string(), base.join("state"), vec!["1.1.1.1".into()]);
        r.git_bin = git.display().to_string();
        (r, log)
    }

    fn spec(id: &str) -> VerifySpec {
        VerifySpec {
            id: id.into(),
            env: EnvSpec {
                id: "v1v2v3v4v5v6v7v8".into(),
                kind: "verify".into(),
                compose: "services:\n  web:\n    build: ./app\n".into(),
                subnet_index: 7,
                public_service: "web".into(),
                source: Some(EnvSource {
                    repo_url: "https://github.com/a/b".into(),
                    commit: "98b705d0123456789abcdef0123456789abcdef0".into(),
                    git_ref: None,
                    compose_dir: String::new(),
                    auth_header: None,
                }),
                build: None,
                release: vec![],
                vars: vec![EnvValue {
                    name: "S".into(),
                    value: "FATAL boom".into(),
                    user: true,
                }],
            },
            public_port: 80,
            test: Some(VerifyCmd {
                image: "node:22-bookworm".into(),
                setup: "npm ci".into(),
                command: "npm test".into(),
            }),
            lint: Some(VerifyCmd {
                image: "node:22-bookworm".into(),
                setup: String::new(),
                command: "npm run lint".into(),
            }),
            smoke: vec!["/".into(), "/boom".into()],
            keep_on_failure: false,
        }
    }

    async fn http_server() -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            while let Ok((mut c, _)) = l.accept().await {
                let mut buf = [0u8; 256];
                let n = c.read(&mut buf).await.unwrap_or(0);
                let code = if String::from_utf8_lossy(&buf[..n]).contains("/boom") {
                    502
                } else {
                    200
                };
                let _ = c.write_all(format!("HTTP/1.1 {code} X\r\n\r\n").as_bytes()).await;
            }
        });
        addr
    }

    type Collected = (Vec<(String, String)>, Vec<Issue>, Vec<String>, Option<(Option<String>, bool)>);

    fn collect(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentMsg>) -> Collected {
        let (mut stages, mut issues, mut logs, mut done) = (vec![], vec![], vec![], None);
        while let Ok(m) = rx.try_recv() {
            match m {
                AgentMsg::VerifyStage { stage, status, .. } => stages.push((stage, status)),
                AgentMsg::VerifyIssue { issue, .. } => issues.push(issue),
                AgentMsg::VerifyLog { line, .. } => logs.push(line),
                AgentMsg::VerifyDone { error, kept, .. } => done = Some((error, kept)),
                AgentMsg::BuildUsage { kind, secs, .. } => assert!(kind == "verify" && secs >= 1),
                m => panic!("{m:?}"),
            }
        }
        (stages, issues, logs, done)
    }

    fn final_status(stages: &[(String, String)], s: &str) -> String {
        stages
            .iter()
            .rev()
            .find(|(x, _)| x == s)
            .map(|(_, st)| st.clone())
            .unwrap_or_default()
    }

    // ADR 0020：完整流程；測試失敗仍跑 smoke、logs、destroy；建置失敗略過需要環境的階段；保留環境
    #[cfg(unix)]
    #[tokio::test]
    async fn runs_all_stages_and_reports() {
        let base = std::env::temp_dir().join(format!("caloud-verify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let (mut r, log) = fake(&base);
        r.smoke_target = Some(http_server().await);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        r.verify(&spec("run1"), &tx).await;
        let (stages, issues, logs, done) = collect(&mut rx);
        assert_eq!(done, Some((None, false)));
        for (s, want) in [
            ("fetch", "passed"),
            ("build", "passed"),
            ("static", "failed"),
            ("start", "passed"),
            ("test", "passed"),
            ("smoke", "failed"),
            ("logs", "failed"),
            ("destroy", "passed"),
        ] {
            assert_eq!(final_status(&stages, s), want, "{s}: {stages:?}");
        }
        let rules: Vec<&str> = issues.iter().map(|i| i.rule.as_str()).collect();
        for want in ["secret/aws-access-key", "vuln/GHSA-crit", "smoke/5xx", "logs/restart", "logs/fatal"] {
            assert!(rules.contains(&want), "{want}: {rules:?}");
        }
        // 秘密變數的值在 log 與問題中遮蔽
        assert!(issues.iter().all(|i| !i.detail.contains("FATAL boom")), "{issues:?}");
        assert!(logs.iter().any(|l| l.contains("ok 3 tests")), "{logs:?}");
        let d = std::fs::read_to_string(&log).unwrap();
        assert!(
            d.contains("--runtime runsc --network caloud-build") && d.contains("node:22-bookworm sh -c npm ci && npm test"),
            "{d}"
        );
        assert!(d.contains("down -v"), "銷毀環境");

        // 測試失敗：High，但 smoke 與 logs 仍執行
        std::fs::write(base.join("TEST_FAIL"), "").unwrap();
        r.verify(&spec("run2"), &tx).await;
        let (stages, issues, _, _) = collect(&mut rx);
        assert_eq!(final_status(&stages, "test"), "failed");
        assert_eq!(final_status(&stages, "smoke"), "failed");
        assert!(issues
            .iter()
            .any(|i| i.rule == "test/failed" && i.severity == "high" && i.detail.contains("1 failing")));
        std::fs::remove_file(base.join("TEST_FAIL")).unwrap();

        // 建置失敗：Blocker；static 與 test 仍執行；start、smoke、logs 略過；保留環境
        std::fs::write(base.join("BUILD_FAIL"), "").unwrap();
        let mut keep = spec("run3");
        keep.keep_on_failure = true;
        std::fs::write(&log, "").unwrap();
        r.verify(&keep, &tx).await;
        let (stages, issues, _, done) = collect(&mut rx);
        assert_eq!(done, Some((None, true)));
        assert!(issues.iter().any(|i| i.rule == "build/failed" && i.severity == "blocker"));
        for (s, want) in [
            ("build", "failed"),
            ("static", "failed"),
            ("start", "skipped"),
            ("test", "passed"),
            ("smoke", "skipped"),
            ("destroy", "skipped"),
        ] {
            assert_eq!(final_status(&stages, s), want, "{s}: {stages:?}");
        }
        assert!(!std::fs::read_to_string(&log).unwrap().contains("down -v"), "保留時不銷毀");
        std::fs::remove_file(base.join("BUILD_FAIL")).unwrap();

        // ADR 0022：GPU 服務 → 環境用 runsc-gpu，測試沙箱帶 --gpus all
        let mut g = spec("run4");
        g.env.compose =
            "services:\n  web:\n    build: ./app\n    deploy: {resources: {reservations: {devices: [{capabilities: [gpu]}]}}}\n".into();
        std::fs::write(&log, "").unwrap();
        r.verify(&g, &tx).await;
        let _ = collect(&mut rx);
        let d = std::fs::read_to_string(&log).unwrap();
        assert!(d.contains("--runtime runsc-gpu --gpus all --network caloud-build"), "{d}");
        let o = std::fs::read_to_string(base.join("state/envs/v1v2v3v4v5v6v7v8/caloud.override.yml")).unwrap_or_default();
        assert!(o.is_empty() || o.contains("runtime: runsc-gpu"), "{o}");
        let _ = std::fs::remove_dir_all(&base);
    }

    // FR-CI-04：取消時終止並銷毀
    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_destroys() {
        let base = std::env::temp_dir().join(format!("caloud-verify-c-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let (r, log) = fake(&base);
        std::fs::write(base.join("SLOW"), "").unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let r2 = r.clone();
        let h = tokio::spawn(async move { r2.verify(&spec("runc"), &tx).await });
        for _ in 0..200 {
            if std::fs::read_to_string(&log).unwrap_or_default().contains("caloud-test-") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        r.cancel_verify("runc");
        tokio::time::timeout(Duration::from_secs(10), h).await.unwrap().unwrap();
        let (_, _, _, done) = collect(&mut rx);
        assert_eq!(done, Some((Some("已取消".into()), false)));
        assert!(std::fs::read_to_string(&log).unwrap().contains("down -v"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
