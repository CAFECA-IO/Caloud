//! API 整合測試（M1-14）：啟動真實 router，外部依賴以替身取代（docs/08-coding-standards.md §2）。
//!
//! - 模擬 CAFECA 錢包：`/.well-known/cafeca-configuration`、JSON-RPC `eth_call`、`/api/profile`
//! - 模擬 DoH：`/dns-query`，TXT 紀錄由測試設定
//! - 模擬 docker：`CALOUD_DOCKER_BIN` 指向替身腳本，記錄參數並可指定失敗
//!
//! 替身 RPC 只在「簽章 == 正確的 EIP-712 SignIn 雜湊」時回傳 ERC-1271 magic，
//! 因此能驗證後端算出的雜湊與送出的參數都正確。
#![forbid(unsafe_code)]

use alloy_primitives::{Address, Bytes};
use alloy_sol_types::{sol, SolCall};
use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use caloud_api::AppState;
use caloud_auth::{CafecaVerifier, SignInMessage};
use caloud_core::config::Config;
use caloud_orchestrator::ComposeRunner;
use caloud_proxy::RouteTable;
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

sol! {
    function isValidSignature(bytes32 hash, bytes signature) external view returns (bytes4);
}

const CHAIN_ID: u64 = 8018;

/// 竄改錢包回應的函式（測試失敗情境用）
type Tamper = Box<dyn FnOnce(&mut SignInMessage, &mut u64) + Send>;

// ───────────────────────── 替身：CAFECA 錢包 + DoH ─────────────────────────

#[derive(Clone, Default)]
struct MockState {
    base: Arc<Mutex<String>>,
    txt: Arc<Mutex<HashMap<String, String>>>,
}

async fn spawn_mock() -> (String, MockState) {
    let st = MockState::default();
    let app = Router::new()
        .route(
            "/.well-known/cafeca-configuration",
            get(|State(s): State<MockState>| async move {
                let base = s.base.lock().unwrap().clone();
                Json(json!({ "issuer": base, "chain": { "id": CHAIN_ID, "rpc": format!("{base}/rpc") }, "contracts": null }))
            }),
        )
        .route(
            "/rpc",
            post(|Json(req): Json<Value>| async move {
                let data = req["params"][0]["data"].as_str().unwrap_or_default();
                let bytes = hex::decode(data.trim_start_matches("0x")).unwrap_or_default();
                let ok = isValidSignatureCall::abi_decode(&bytes)
                    .map(|c| c.signature.as_ref() == c.hash.as_slice())
                    .unwrap_or(false);
                let result = if ok {
                    format!("0x1626ba7e{}", "0".repeat(56))
                } else {
                    format!("0xffffffff{}", "0".repeat(56))
                };
                Json(json!({ "jsonrpc": "2.0", "id": req["id"], "result": result }))
            }),
        )
        .route(
            "/api/profile",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                let a = q.get("q").cloned().unwrap_or_default();
                Json(json!({ "handle": format!("user_{}", &a[a.len() - 4..]) }))
            }),
        )
        .route(
            "/dns-query",
            get(|State(s): State<MockState>, Query(q): Query<HashMap<String, String>>| async move {
                let name = q.get("name").cloned().unwrap_or_default();
                let answer: Vec<Value> = s
                    .txt
                    .lock()
                    .unwrap()
                    .get(&name)
                    .map(|v| vec![json!({ "name": name, "type": 16, "data": format!("\"{v}\"") })])
                    .unwrap_or_default();
                Json(json!({ "Status": 0, "Answer": answer }))
            }),
        )
        .with_state(st.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    *st.base.lock().unwrap() = base.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, st)
}

// ───────────────────────── 測試環境 ─────────────────────────

struct Env {
    base: String,
    origin: String,
    mock: MockState,
    routes: RouteTable,
    db: caloud_core::db::Db,
    dir: PathBuf,
    http: reqwest::Client,
    tunnels: caloud_proxy::TunnelRegistry,
}

impl Env {
    async fn new() -> Self {
        let (mock_base, mock) = spawn_mock().await;
        let dir = std::env::temp_dir().join(format!("caloud-it-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // 替身 docker：記錄每次呼叫；存在 fail 檔時以錯誤結束；ps 回傳一個容器
        let docker = dir.join("fake-docker.sh");
        std::fs::write(
            &docker,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{log}'\nif [ -f '{fail}' ]; then echo 'image pull failed: not found' >&2; exit 1; fi\n\
                 case \"$*\" in *' ps '*) echo '{{\"Service\":\"web\",\"Name\":\"caloud-x-web-1\",\"State\":\"running\",\"Health\":\"\"}}';; \
                 *' logs '*) echo 'web-1 | ready';; esac\nexit 0\n",
                log = dir.join("docker.log").display(),
                fail = dir.join("docker.fail").display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // 替身 git：checkout 時把 gitrepo/ 的內容複製到工作目錄；網址含 missing 時失敗
        let git = dir.join("fake-git.sh");
        std::fs::write(
            &git,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{log}'\nsrc=$2\ncase \"$*\" in *missing*) echo 'repository not found' >&2; exit 128;; *' checkout '*) cp -R '{repo}/.' \"$src\";; esac\nexit 0\n",
                log = dir.join("git.log").display(),
                repo = dir.join("gitrepo").display()
            ),
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("gitrepo")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let origin = format!("http://{addr}");
        let cfg = Config {
            public_origin: origin.clone(),
            api_listen: addr.to_string(),
            // 測試只啟動 API router；代理埠用正式值，讓保留連接埠（FR-APP-16）與正式環境一致
            http_listen: "0.0.0.0:80".into(),
            https_listen: "0.0.0.0:443".into(),
            cafeca_wallet: mock_base.clone(),
            cafeca_rpc: None,
            data_dir: dir.join("data"),
            acme_email: None,
            acme_staging: true,
            edge_network: "caloud-edge-test".into(),
            default_role: "pending".into(),
            docker_bin: docker.display().to_string(),
            doh_url: format!("{mock_base}/dns-query"),
            git_bin: git.display().to_string(),
        };
        let db = caloud_core::db::connect(&cfg.data_dir).await.unwrap();
        let routes = RouteTable::default();
        let tunnels = caloud_proxy::TunnelRegistry::default();
        let state = AppState {
            verifier: Arc::new(CafecaVerifier::new(cfg.cafeca_wallet.clone(), None)),
            runner: ComposeRunner::new(
                cfg.data_dir.join("apps"),
                cfg.edge_network.clone(),
                cfg.docker_bin.clone(),
                cfg.git_bin.clone(),
            ),
            routes: routes.clone(),
            db: db.clone(),
            http: reqwest::Client::new(),
            cfg: Arc::new(cfg),
            agents: Default::default(),
            tunnels: tunnels.clone(),
        };
        tokio::spawn(async move { axum::serve(listener, caloud_api::router(state)).await.unwrap() });
        Env {
            base: origin.clone(),
            origin,
            mock,
            routes,
            db,
            dir,
            http: reqwest::Client::new(),
            tunnels,
        }
    }

    fn url(&self, p: &str) -> String {
        format!("{}{}", self.base, p)
    }

    /// 組出錢包回應；`tamper` 用來製造各種失敗情境
    async fn signin_response(&self, account: Address, tamper: impl FnOnce(&mut SignInMessage, &mut u64)) -> Value {
        let nonce: Value = self
            .http
            .post(self.url("/api/auth/nonce"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut msg = SignInMessage {
            domain: self.origin.clone(),
            uri: format!("{}/", self.origin),
            nonce: nonce["nonce"].as_str().unwrap().to_string(),
            issued_at: now,
            expires_at: now + 300,
            statement: "登入 Caloud 管理介面".into(),
            claims: "handle".into(),
            channel: String::new(),
        };
        let mut chain = CHAIN_ID;
        let hash = caloud_auth::signin_hash(&msg, CHAIN_ID, account); // 以「正確」的內容簽署
        tamper(&mut msg, &mut chain);
        json!({ "v": 1, "type": "cafeca:auth", "account": account, "chainId": chain, "message": msg,
                "signature": Bytes::from(hash.to_vec()), "claims": {} })
    }

    /// 登入並回傳 session cookie
    async fn login(&self, account: Address) -> (String, Value) {
        let body = self.signin_response(account, |_, _| {}).await;
        let res = self.http.post(self.url("/api/auth/login")).json(&body).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "login failed: {:?}", res.text().await);
        let cookie = res.headers()["set-cookie"].to_str().unwrap().to_string();
        let user: Value = res.json().await.unwrap();
        (cookie.split(';').next().unwrap().to_string(), user)
    }

    async fn call(&self, method: &str, path: &str, cookie: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut req = self.http.request(method.parse().unwrap(), self.url(path)).header("cookie", cookie);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.unwrap();
        let status = res.status();
        let text = res.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    fn docker_log(&self) -> String {
        std::fs::read_to_string(self.dir.join("docker.log")).unwrap_or_default()
    }

    async fn wait_app_status(&self, cookie: &str, id: &str, want: &str) -> Value {
        for _ in 0..50 {
            let (_, apps) = self.call("GET", "/api/apps", cookie, None).await;
            let app = apps.as_array().unwrap().iter().find(|a| a["id"] == id).cloned().unwrap();
            if app["status"] == want {
                return app;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("app {id} never reached {want}");
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn acct(b: u8) -> Address {
    Address::repeat_byte(b)
}

/// 建立 super admin 與一位已核准的 member
async fn admin_and_member(env: &Env) -> (String, String) {
    let (admin, _) = env.login(acct(0xa1)).await;
    let (member, m) = env.login(acct(0xb2)).await;
    let (st, _) = env
        .call(
            "PUT",
            &format!("/api/users/{}", m["account"].as_str().unwrap()),
            &admin,
            Some(json!({ "role": "member" })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    (admin, member)
}

// ───────────────────────── 身分驗證 ─────────────────────────

// FR-AUTH-01、FR-AUTH-03、FR-AUTH-04、FR-AUTH-05、FR-AUTH-07
#[tokio::test]
async fn first_login_is_super_admin_and_session_cookie_is_hardened() {
    let env = Env::new().await;
    let body = env.signin_response(acct(0xa1), |_, _| {}).await;
    let res = env.http.post(env.url("/api/auth/login")).json(&body).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cookie = res.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"), "{cookie}");
    let first: Value = res.json().await.unwrap();
    assert_eq!(first["role"], "super_admin");
    assert_eq!(first["account"], format!("{:#x}", acct(0xa1)));
    assert_eq!(first["handle"], "user_a1a1", "代稱需由錢包 API 查詢");

    let (_, second) = env.login(acct(0xb2)).await;
    assert_eq!(second["role"], "pending");

    // DB 只存 token 雜湊
    let raw = cookie.split(';').next().unwrap().split('=').nth(1).unwrap().to_string();
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE token_hash = ?")
        .bind(&raw)
        .fetch_one(&env.db)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

// FR-AUTH-01：網域、chainId、有效期、簽章任一不符都拒絕
#[tokio::test]
async fn login_rejects_tampered_responses() {
    let env = Env::new().await;
    let cases: Vec<(&str, Tamper)> = vec![
        ("網域", Box::new(|m, _| m.domain = "https://evil.example".into())),
        ("chainId", Box::new(|_, c| *c = 1)),
        ("過期", Box::new(|m, _| m.expires_at = 1)),
        ("簽章", Box::new(|m, _| m.statement = "被竄改".into())),
    ];
    for (name, tamper) in cases {
        let body = env.signin_response(acct(0xa1), tamper).await;
        let res = env.http.post(env.url("/api/auth/login")).json(&body).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "case {name}");
    }
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(&env.db).await.unwrap();
    assert_eq!(n, 0, "失敗的登入不可建立使用者");
}

// FR-AUTH-02：nonce 只能用一次
#[tokio::test]
async fn nonce_is_single_use() {
    let env = Env::new().await;
    let body = env.signin_response(acct(0xa1), |_, _| {}).await;
    let first = env.http.post(env.url("/api/auth/login")).json(&body).send().await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let replay = env.http.post(env.url("/api/auth/login")).json(&body).send().await.unwrap();
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
}

// FR-AUTH-06
#[tokio::test]
async fn logout_invalidates_session() {
    let env = Env::new().await;
    let (cookie, _) = env.login(acct(0xa1)).await;
    assert_eq!(env.call("GET", "/api/me", &cookie, None).await.0, StatusCode::OK);
    assert_eq!(env.call("POST", "/api/auth/logout", &cookie, None).await.0, StatusCode::OK);
    assert_eq!(env.call("GET", "/api/me", &cookie, None).await.0, StatusCode::UNAUTHORIZED);
}

// ───────────────────────── 使用者與權限 ─────────────────────────

// FR-USER-01、FR-USER-02、FR-USER-04、NFR-SEC-02
#[tokio::test]
async fn role_rules_are_enforced() {
    let env = Env::new().await;
    let (admin, a) = env.login(acct(0xa1)).await;
    let (pending, p) = env.login(acct(0xb2)).await;
    let admin_acc = a["account"].as_str().unwrap().to_string();
    let pending_acc = p["account"].as_str().unwrap().to_string();

    // pending 不能看使用者、不能建立資源
    assert_eq!(env.call("GET", "/api/users", &pending, None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        env.call(
            "POST",
            "/api/apps",
            &pending,
            Some(json!({ "compose_yaml": "services: {web: {image: nginx}}" }))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        env.call("POST", "/api/domains", &pending, Some(json!({ "hostname": "a.example.com" })))
            .await
            .0,
        StatusCode::FORBIDDEN
    );

    // super admin 不可被變更、不可授予 super_admin、不可改自己
    assert_eq!(
        env.call("PUT", &format!("/api/users/{admin_acc}"), &admin, Some(json!({ "role": "member" })))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        env.call(
            "PUT",
            &format!("/api/users/{pending_acc}"),
            &admin,
            Some(json!({ "role": "super_admin" }))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );

    // super admin 授予 admin；admin 不能撤銷其他 admin
    let (st, u) = env
        .call(
            "PUT",
            &format!("/api/users/{pending_acc}"),
            &admin,
            Some(json!({ "role": "admin" })),
        )
        .await;
    assert_eq!((st, u["role"].as_str()), (StatusCode::OK, Some("admin")));
    let (_, c) = env.login(acct(0xc3)).await;
    let c_acc = c["account"].as_str().unwrap();
    assert_eq!(
        env.call("PUT", &format!("/api/users/{c_acc}"), &pending, Some(json!({ "role": "admin" })))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        env.call("PUT", &format!("/api/users/{c_acc}"), &pending, Some(json!({ "role": "member" })))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        env.call(
            "PUT",
            &format!("/api/users/{pending_acc}"),
            &pending,
            Some(json!({ "role": "member" }))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );

    let (st, list) = env.call("GET", "/api/users", &admin, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 3);
}

// FR-USER-03
#[tokio::test]
async fn disabling_user_revokes_sessions() {
    let env = Env::new().await;
    let (admin, _) = env.login(acct(0xa1)).await;
    let (other, o) = env.login(acct(0xb2)).await;
    assert_eq!(env.call("GET", "/api/me", &other, None).await.0, StatusCode::OK);
    let path = format!("/api/users/{}", o["account"].as_str().unwrap());
    assert_eq!(
        env.call("PUT", &path, &admin, Some(json!({ "role": "disabled" }))).await.0,
        StatusCode::OK
    );
    assert_eq!(env.call("GET", "/api/me", &other, None).await.0, StatusCode::UNAUTHORIZED);
    // 停用者重新登入也被拒絕
    let body = env.signin_response(acct(0xb2), |_, _| {}).await;
    assert_eq!(
        env.http.post(env.url("/api/auth/login")).json(&body).send().await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}

// ───────────────────────── App ─────────────────────────

// FR-APP-02
#[tokio::test]
async fn unsafe_compose_is_rejected_with_reason() {
    let env = Env::new().await;
    let (admin, _) = env.login(acct(0xa1)).await;
    let (st, err) = env
        .call(
            "POST",
            "/api/apps",
            &admin,
            Some(json!({ "compose_yaml": "services:\n  web:\n    image: nginx\n    ports: ['80:80']\n" })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let msg = err["error"].as_str().unwrap();
    assert!(msg.contains("`web`") && msg.contains("`ports`"), "{msg}");
    assert!(env.docker_log().is_empty(), "不合規的 compose 不可呼叫 docker");
}

// FR-APP-01、FR-APP-03、FR-APP-05、FR-APP-06、FR-APP-07、FR-AUD-01
#[tokio::test]
async fn app_lifecycle_runs_compose_with_isolated_project() {
    let env = Env::new().await;
    let (_, member) = admin_and_member(&env).await;
    let yaml = "services:\n  web:\n    image: nginx:alpine\n  db:\n    image: postgres:17\n";
    let (st, app) = env
        .call("POST", "/api/apps", &member, Some(json!({ "name": "blog", "compose_yaml": yaml })))
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let id = app["id"].as_str().unwrap().to_string();
    env.wait_app_status(&member, &id, "running").await;

    let project = caloud_core::project_name(&id);
    let log = env.docker_log();
    assert!(log.contains(&format!("compose -p {project} --project-directory")), "{log}");
    assert!(log.contains("up -d --remove-orphans --wait"), "{log}");

    // override 讓每個 service 加入 edge 網路並設 alias（FR-APP-07）
    let ov = std::fs::read_to_string(env.dir.join("data/apps").join(&id).join("caloud.override.yml")).unwrap();
    assert!(
        ov.contains("caloud-edge-test") && ov.contains(&caloud_core::model_upstream_host(&id, "db")),
        "{ov}"
    );

    let (st, detail) = env.call("GET", &format!("/api/apps/{id}"), &member, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(detail["services"], json!(["web", "db"]));
    assert_eq!(detail["containers"][0]["State"], "running");

    let (st, logs) = env.call("GET", &format!("/api/apps/{id}/logs?tail=999999"), &member, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(logs.as_str().unwrap().contains("ready"));
    assert!(env.docker_log().contains("--tail 5000"), "tail 上限 5000（FR-APP-06）");

    assert_eq!(
        env.call("POST", &format!("/api/apps/{id}/stop"), &member, None).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        env.call("DELETE", &format!("/api/apps/{id}"), &member, None).await.0,
        StatusCode::NO_CONTENT
    );
    let log = env.docker_log();
    assert!(
        log.contains("down --remove-orphans") && !log.contains("--volumes"),
        "刪除預設保留 volume（FR-APP-05）：{log}"
    );

    let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log ORDER BY id")
        .fetch_all(&env.db)
        .await
        .unwrap();
    for a in ["user_login", "user_set_role", "app_create", "app_delete"] {
        assert!(actions.iter().any(|x| x == a), "缺少稽核 {a}：{actions:?}");
    }
}

// FR-APP-03：部署失敗保存錯誤訊息
#[tokio::test]
async fn failed_deploy_records_error() {
    let env = Env::new().await;
    let (admin, _) = env.login(acct(0xa1)).await;
    std::fs::write(env.dir.join("docker.fail"), "").unwrap();
    let (_, app) = env
        .call(
            "POST",
            "/api/apps",
            &admin,
            Some(json!({ "compose_yaml": "services: {web: {image: nope}}" })),
        )
        .await;
    let app = env.wait_app_status(&admin, app["id"].as_str().unwrap(), "failed").await;
    assert!(app["last_error"].as_str().unwrap().contains("image pull failed"), "{app}");
}

// NFR-SEC-02：member 不能操作他人的 App
#[tokio::test]
async fn member_cannot_touch_others_apps() {
    let env = Env::new().await;
    let (admin, member) = admin_and_member(&env).await;
    let (_, app) = env
        .call(
            "POST",
            "/api/apps",
            &admin,
            Some(json!({ "compose_yaml": "services: {web: {image: nginx}}", "deploy": false })),
        )
        .await;
    let id = app["id"].as_str().unwrap();
    for (m, p) in [
        ("GET", format!("/api/apps/{id}")),
        ("POST", format!("/api/apps/{id}/deploy")),
        ("DELETE", format!("/api/apps/{id}")),
    ] {
        assert_eq!(env.call(m, &p, &member, None).await.0, StatusCode::FORBIDDEN, "{m} {p}");
    }
    let (_, list) = env.call("GET", "/api/apps", &member, None).await;
    assert!(list.as_array().unwrap().is_empty(), "member 只看得到自己的 App");
}

// ───────────────────────── 網域與路由 ─────────────────────────

// FR-DOM-01
#[tokio::test]
async fn domain_is_normalized_and_unique() {
    let env = Env::new().await;
    let (admin, _) = env.login(acct(0xa1)).await;
    let (st, d) = env
        .call("POST", "/api/domains", &admin, Some(json!({ "hostname": " Blog.Example.COM. " })))
        .await;
    assert_eq!((st, d["hostname"].as_str()), (StatusCode::CREATED, Some("blog.example.com")));
    assert_eq!(
        env.call("POST", "/api/domains", &admin, Some(json!({ "hostname": "blog.example.com" })))
            .await
            .0,
        StatusCode::CONFLICT
    );
    for bad in ["localhost", "-a.example.com", "a..example.com", "a_b.example.com"] {
        assert_eq!(
            env.call("POST", "/api/domains", &admin, Some(json!({ "hostname": bad }))).await.0,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
}

// FR-DOM-02、FR-DOM-03、FR-DOM-04
#[tokio::test]
async fn verified_and_bound_domain_enters_route_table() {
    let env = Env::new().await;
    let (admin, _) = env.login(acct(0xa1)).await;
    let (_, app) = env
        .call(
            "POST",
            "/api/apps",
            &admin,
            Some(json!({ "compose_yaml": "services: {web: {image: nginx}}" })),
        )
        .await;
    let id = app["id"].as_str().unwrap().to_string();
    env.wait_app_status(&admin, &id, "running").await;

    let (_, d) = env
        .call("POST", "/api/domains", &admin, Some(json!({ "hostname": "blog.example.com" })))
        .await;
    let token = d["verify_token"].as_str().unwrap().to_string();

    // 綁定不存在的 service → 400
    let (st, _) = env
        .call(
            "PUT",
            "/api/domains/blog.example.com/bind",
            &admin,
            Some(json!({ "app_id": id, "service": "db", "port": 80 })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = env
        .call(
            "PUT",
            "/api/domains/blog.example.com/bind",
            &admin,
            Some(json!({ "app_id": id, "service": "web", "port": 80 })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert!(env.routes.lookup("blog.example.com").is_none(), "未驗證的網域不可進入路由表");

    // 沒有 TXT → 400；設定 TXT 後 → 驗證成功並立即進入路由表
    assert_eq!(
        env.call("POST", "/api/domains/blog.example.com/verify", &admin, None).await.0,
        StatusCode::BAD_REQUEST
    );
    env.mock
        .txt
        .lock()
        .unwrap()
        .insert("_caloud-challenge.blog.example.com".into(), token);
    let (st, d) = env.call("POST", "/api/domains/blog.example.com/verify", &admin, None).await;
    assert_eq!((st, &d["verified"]), (StatusCode::OK, &json!(true)));
    let upstream = format!("http://{}:80", caloud_core::model_upstream_host(&id, "web"));
    assert_eq!(env.routes.lookup("Blog.Example.com:443").as_deref(), Some(upstream.as_str()));

    // App 停止 → 路由移除；解除綁定亦同
    env.call("POST", &format!("/api/apps/{id}/stop"), &admin, None).await;
    assert!(env.routes.lookup("blog.example.com").is_none());
    env.call("POST", &format!("/api/apps/{id}/deploy"), &admin, None).await;
    env.wait_app_status(&admin, &id, "running").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(env.routes.lookup("blog.example.com").is_some());
    env.call("PUT", "/api/domains/blog.example.com/bind", &admin, Some(json!({ "app_id": null })))
        .await;
    assert!(env.routes.lookup("blog.example.com").is_none());
}

// ───────────────────────── API 權杖與 MCP（ADR 0008） ─────────────────────────

impl Env {
    async fn create_token(&self, cookie: &str, name: &str) -> String {
        let (st, v) = self.call("POST", "/api/tokens", cookie, Some(json!({ "name": name }))).await;
        assert_eq!(st, StatusCode::CREATED, "{v}");
        v["token"].as_str().unwrap().to_string()
    }

    async fn mcp_raw(&self, token: Option<&str>, body: &str, extra: &[(&str, &str)]) -> (StatusCode, reqwest::header::HeaderMap, Value) {
        let mut req = self
            .http
            .post(self.url("/mcp"))
            .header("content-type", "application/json")
            .body(body.to_string());
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        let res = req.send().await.unwrap();
        let (st, h) = (res.status(), res.headers().clone());
        let text = res.text().await.unwrap();
        (st, h, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn rpc(&self, token: &str, method: &str, params: Value) -> Value {
        let body = json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params }).to_string();
        let (st, _, v) = self.mcp_raw(Some(token), &body, &[]).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["id"], 7);
        v
    }

    async fn tool(&self, token: &str, name: &str, args: Value) -> Value {
        self.rpc(token, "tools/call", json!({ "name": name, "arguments": args })).await["result"].clone()
    }
}

// FR-MCP-02：權杖只顯示一次、列表不含權杖、可撤銷；pending 不能建立
#[tokio::test]
async fn api_tokens_are_shown_once_and_revocable() {
    let env = Env::new().await;
    let (admin, _) = env.login(acct(0xa1)).await;
    let (pending, _) = env.login(acct(0xb2)).await;
    assert_eq!(
        env.call("POST", "/api/tokens", &pending, Some(json!({ "name": "x" }))).await.0,
        StatusCode::FORBIDDEN
    );
    for bad in [
        json!({ "name": "" }),
        json!({ "name": "x", "expires_in_days": 0 }),
        json!({ "name": "x", "expires_in_days": 366 }),
    ] {
        assert_eq!(
            env.call("POST", "/api/tokens", &admin, Some(bad.clone())).await.0,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }

    let token = env.create_token(&admin, "Claude Desktop").await;
    assert!(token.starts_with("cld_") && token.len() > 40);
    let (_, list) = env.call("GET", "/api/tokens", &admin, None).await;
    let row = &list[0];
    assert_eq!(row["name"], "Claude Desktop");
    assert_eq!(row["prefix"], &token[..8]);
    assert!(!list.to_string().contains(&token), "列表不可包含權杖本身");
    assert!(row["last_used_at"].is_null());

    env.rpc(&token, "ping", json!({})).await;
    let (_, list) = env.call("GET", "/api/tokens", &admin, None).await;
    assert!(list[0]["last_used_at"].is_i64(), "使用後記錄最後使用時間");

    let id = row["id"].as_str().unwrap();
    assert_eq!(
        env.call("DELETE", &format!("/api/tokens/{id}"), &pending, None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        env.call("DELETE", &format!("/api/tokens/{id}"), &admin, None).await.0,
        StatusCode::NO_CONTENT
    );
    let (st, _, _) = env.mcp_raw(Some(&token), r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, &[]).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "撤銷後立即失效");
}

// FR-MCP-01、FR-MCP-03：驗證、Origin、協定錯誤與版本協商
#[tokio::test]
async fn mcp_transport_auth_and_protocol() {
    let env = Env::new().await;
    let (admin, _) = env.login(acct(0xa1)).await;
    let token = env.create_token(&admin, "test").await;
    let ping = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;

    let (st, h, _) = env.mcp_raw(None, ping, &[]).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert!(h["www-authenticate"].to_str().unwrap().starts_with("Bearer"));
    let (st, _, _) = env.mcp_raw(None, ping, &[("cookie", &admin)]).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "/mcp 不接受 cookie");
    let (st, _, _) = env.mcp_raw(Some(&token), ping, &[("origin", "https://evil.example")]).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _, _) = env.mcp_raw(Some(&token), ping, &[("origin", &env.origin)]).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        env.http.get(env.url("/mcp")).send().await.unwrap().status(),
        StatusCode::METHOD_NOT_ALLOWED
    );

    let (st, _, v) = env.mcp_raw(Some(&token), "{not json", &[]).await;
    assert_eq!((st, v["error"]["code"].as_i64()), (StatusCode::BAD_REQUEST, Some(-32700)));
    let (_, _, v) = env
        .mcp_raw(Some(&token), r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#, &[])
        .await;
    assert_eq!(v["error"]["code"], -32600);
    let (st, _, _) = env
        .mcp_raw(Some(&token), r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, &[])
        .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    assert_eq!(env.rpc(&token, "resources/list", json!({})).await["error"]["code"], -32601);

    let init = env
        .rpc(
            &token,
            "initialize",
            json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } }),
        )
        .await;
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "caloud");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    let init = env.rpc(&token, "initialize", json!({ "protocolVersion": "1999-01-01" })).await;
    assert_eq!(init["result"]["protocolVersion"], caloud_api::MCP_PROTOCOL_VERSIONS[0]);
}

// FR-MCP-03、FR-MCP-04：工具清單依角色過濾；權限隨角色即時變化
#[tokio::test]
async fn mcp_tools_follow_user_role() {
    let env = Env::new().await;
    let (admin, _) = env.login(acct(0xa1)).await;
    let (member, m) = env.login(acct(0xb2)).await;
    let m_path = format!("/api/users/{}", m["account"].as_str().unwrap());
    env.call("PUT", &m_path, &admin, Some(json!({ "role": "member" }))).await;
    let admin_token = env.create_token(&admin, "admin").await;
    let member_token = env.create_token(&member, "member").await;

    let names = |v: Value| {
        v["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    let admin_tools = names(env.rpc(&admin_token, "tools/list", json!({})).await);
    let member_tools = names(env.rpc(&member_token, "tools/list", json!({})).await);
    assert!(admin_tools.contains(&"set_user_role".to_string()));
    assert!(!member_tools.contains(&"list_users".to_string()) && member_tools.contains(&"create_app".to_string()));
    assert_eq!(
        env.rpc(&member_token, "tools/call", json!({ "name": "list_users" })).await["error"]["code"],
        -32602
    );

    let who = env.tool(&member_token, "whoami", json!({})).await;
    assert_eq!(who["structuredContent"]["role"], "member");

    // 角色改為 pending → 權杖立即失效；改回 member → 恢復
    env.call("PUT", &m_path, &admin, Some(json!({ "role": "pending" }))).await;
    let (st, _, _) = env
        .mcp_raw(Some(&member_token), r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, &[])
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    env.call("PUT", &m_path, &admin, Some(json!({ "role": "member" }))).await;
    env.rpc(&member_token, "ping", json!({})).await;

    // admin 透過 MCP 變更角色，規則與 REST 相同
    let r = env
        .tool(
            &admin_token,
            "set_user_role",
            json!({ "account": m["account"], "role": "disabled" }),
        )
        .await;
    assert_eq!(r["structuredContent"]["role"], "disabled");
    let (st, _, _) = env
        .mcp_raw(Some(&member_token), r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, &[])
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

// FR-MCP-04～07：App 與網域完整流程、錯誤回報、刪除確認、稽核
#[tokio::test]
async fn mcp_tools_manage_apps_and_domains() {
    let env = Env::new().await;
    let (admin, _) = env.login(acct(0xa1)).await;
    let token = env.create_token(&admin, "Claude Code").await;

    // 參數錯誤 → 協定錯誤
    let v = env
        .rpc(
            &token,
            "tools/call",
            json!({ "name": "create_app", "arguments": { "compose": "x" } }),
        )
        .await;
    assert_eq!(v["error"]["code"], -32602);
    let v = env
        .rpc(&token, "tools/call", json!({ "name": "create_app", "arguments": {} }))
        .await;
    assert_eq!(v["error"]["code"], -32602);

    // 業務錯誤 → isError，訊息同 REST
    let r = env
        .tool(
            &token,
            "create_app",
            json!({ "compose_yaml": "services: {web: {image: nginx, privileged: true}}" }),
        )
        .await;
    assert_eq!(r["isError"], true);
    assert!(r["content"][0]["text"].as_str().unwrap().contains("`privileged`"));

    let r = env
        .tool(
            &token,
            "create_app",
            json!({ "name": "blog", "compose_yaml": "services: {web: {image: nginx}}" }),
        )
        .await;
    assert_eq!(r["isError"], false, "{r}");
    let id = r["structuredContent"]["id"].as_str().unwrap().to_string();
    assert!(r["content"][0]["text"].as_str().unwrap().contains(&id));
    env.wait_app_status(&admin, &id, "running").await;

    let r = env.tool(&token, "list_apps", json!({ "status": "running" })).await;
    assert_eq!(r["structuredContent"]["apps"][0]["name"], "blog");
    let r = env.tool(&token, "get_app_logs", json!({ "app_id": id, "tail": 10 })).await;
    assert!(r["structuredContent"]["logs"].as_str().unwrap().contains("ready"));

    let r = env.tool(&token, "create_domain", json!({ "hostname": "Blog.Example.com" })).await;
    let recs = &r["structuredContent"]["dns_records"];
    assert_eq!(recs[1]["name"], "_caloud-challenge.blog.example.com");
    env.mock.txt.lock().unwrap().insert(
        "_caloud-challenge.blog.example.com".into(),
        recs[1]["value"].as_str().unwrap().into(),
    );
    assert_eq!(
        env.tool(&token, "verify_domain", json!({ "hostname": "blog.example.com" })).await["structuredContent"]["verified"],
        true
    );
    let r = env
        .tool(
            &token,
            "bind_domain",
            json!({ "hostname": "blog.example.com", "app_id": id, "service": "web", "port": 8080 }),
        )
        .await;
    assert_eq!(r["isError"], false, "{r}");
    assert!(env.routes.lookup("blog.example.com").unwrap().ends_with(":8080"));

    // 刪除需確認
    let r = env
        .tool(&token, "delete_domain", json!({ "hostname": "blog.example.com", "confirm": false }))
        .await;
    assert_eq!(r["isError"], true);
    assert!(env.routes.lookup("blog.example.com").is_some());
    let r = env.tool(&token, "delete_app", json!({ "app_id": id, "confirm": true })).await;
    assert_eq!(r["structuredContent"]["deleted"], id.as_str());
    assert!(env.routes.lookup("blog.example.com").is_none());

    // FR-MCP-07：稽核標示來源與權杖名稱
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT action, detail FROM audit_log WHERE action IN ('app_create','app_delete','domain_bind','token_create')")
            .fetch_all(&env.db)
            .await
            .unwrap();
    for (action, detail) in &rows {
        if action != "token_create" {
            assert!(
                detail.as_deref().unwrap_or_default().contains("via=mcp:Claude Code"),
                "{action}: {detail:?}"
            );
        }
    }
    assert!(rows.iter().any(|(a, _)| a == "token_create"));
}

// ───────────────────────── Git 來源、特權 App、TLS 直通（ADR 0009） ─────────────────────────

const LIVEKIT_COMPOSE: &str = "services:
  init:
    build: .
    env_file: .env
    restart: 'no'
  caddy:
    image: caddy:2
    network_mode: host
  relay:
    build: {context: ., dockerfile: Dockerfile}
    ports: ['80:80', '443:443', '7842:7842/udp']
";

// FR-APP-13～16：Git 來源只限 Admin；特權政策；.env 只回傳名稱；指定 service；保留連接埠移除
#[tokio::test]
async fn git_privileged_app_builds_from_repo() {
    let env = Env::new().await;
    let (admin, member) = admin_and_member(&env).await;
    std::fs::create_dir_all(env.dir.join("gitrepo/deploy/livekit")).unwrap();
    std::fs::write(env.dir.join("gitrepo/deploy/livekit/docker-compose.yml"), LIVEKIT_COMPOSE).unwrap();
    let body = json!({
        "source": "git", "git_url": "https://github.com/CAFECA-IO/SwarmStorage", "git_ref": "main", "git_path": "deploy/livekit",
        "env": "LIVEKIT_DOMAIN=livekit.cafeca.io\n# 註解\nACME_EMAIL=ops@example.com\n", "services": ["caddy", "relay"], "privileged": true,
    });

    // member 不可使用 Git 來源或特權
    assert_eq!(
        env.call("POST", "/api/apps", &member, Some(body.clone())).await.0,
        StatusCode::FORBIDDEN
    );
    let (st, _) = env
        .call(
            "POST",
            "/api/apps",
            &member,
            Some(json!({ "compose_yaml": "services: {a: {image: x, network_mode: host}}", "privileged": true })),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // 非特權的 Git App 不可 build；不合法的網址被拒絕
    let mut plain = body.clone();
    plain["privileged"] = json!(false);
    let (st, err) = env.call("POST", "/api/apps", &admin, Some(plain)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{err}");
    for url in ["http://github.com/x", "https://user:pw@github.com/x", "file:///etc"] {
        let mut b = body.clone();
        b["git_url"] = json!(url);
        assert_eq!(
            env.call("POST", "/api/apps", &admin, Some(b)).await.0,
            StatusCode::BAD_REQUEST,
            "{url}"
        );
    }
    let mut b = body.clone();
    b["git_url"] = json!("https://github.com/CAFECA-IO/missing");
    let (st, err) = env.call("POST", "/api/apps", &admin, Some(b)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(err["error"].as_str().unwrap().contains("無法取得"), "{err}");

    let (st, app) = env.call("POST", "/api/apps", &admin, Some(body)).await;
    assert_eq!(st, StatusCode::CREATED, "{app}");
    assert_eq!(app["env_keys"], json!(["LIVEKIT_DOMAIN", "ACME_EMAIL"]));
    assert!(!app.to_string().contains("ops@example.com"), "API 不可回傳環境變數值");
    assert_eq!(app["services"], json!(["caddy", "relay"]));
    assert_eq!((app["source"].as_str(), app["privileged"].as_bool()), (Some("git"), Some(true)));
    let id = app["id"].as_str().unwrap().to_string();
    env.wait_app_status(&admin, &id, "running").await;

    let proj = env.dir.join("data/apps").join(&id).join("src/deploy/livekit");
    assert!(std::fs::read_to_string(proj.join(".env"))
        .unwrap()
        .contains("ACME_EMAIL=ops@example.com"));
    let log = env.docker_log();
    assert!(log.contains(&format!("--project-directory {}", proj.display())), "{log}");
    assert!(
        log.contains("--build") && log.contains("--wait-timeout 300 --build caddy relay"),
        "{log}"
    );
    let ov = std::fs::read_to_string(env.dir.join("data/apps").join(&id).join("caloud.override.yml")).unwrap();
    assert!(
        ov.contains("!override") && ov.contains("7842:7842/udp") && !ov.contains("443:443"),
        "{ov}"
    );
    assert!(!ov.contains("mem_limit"), "特權 App 不設資源上限");

    // 更新只換 env，省略的欄位維持原值
    let (st, app) = env
        .call(
            "PUT",
            &format!("/api/apps/{id}"),
            &admin,
            Some(json!({ "env": "TURN_DOMAIN=turn.cafeca.io", "deploy": false })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{app}");
    assert_eq!(app["env_keys"], json!(["TURN_DOMAIN"]));
    assert_eq!(app["services"], json!(["caddy", "relay"]));
    assert_eq!(app["git_path"], "deploy/livekit");
}

// FR-DOM-09、FR-DOM-10：TLS 直通與 host 網路 service 的轉發目標
#[tokio::test]
async fn passthrough_domain_routes_by_sni_and_host_gateway() {
    let env = Env::new().await;
    let (admin, _) = env.login(acct(0xa1)).await;
    std::fs::create_dir_all(env.dir.join("gitrepo")).unwrap();
    std::fs::write(env.dir.join("gitrepo/docker-compose.yml"), LIVEKIT_COMPOSE).unwrap();
    let (_, app) = env
        .call(
            "POST",
            "/api/apps",
            &admin,
            Some(json!({ "git_url": "https://github.com/CAFECA-IO/x", "privileged": true })),
        )
        .await;
    let id = app["id"].as_str().unwrap().to_string();
    env.wait_app_status(&admin, &id, "running").await;

    for host in ["livekit.example.com", "relay.example.com"] {
        let (_, d) = env.call("POST", "/api/domains", &admin, Some(json!({ "hostname": host }))).await;
        env.mock
            .txt
            .lock()
            .unwrap()
            .insert(format!("_caloud-challenge.{host}"), d["verify_token"].as_str().unwrap().into());
        env.call("POST", &format!("/api/domains/{host}/verify"), &admin, None).await;
    }
    // 直通需要 tls_port
    let (st, _) = env
        .call(
            "PUT",
            "/api/domains/livekit.example.com/bind",
            &admin,
            Some(json!({ "app_id": id, "service": "caddy", "port": 8088, "tls_mode": "passthrough" })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, d) = env
        .call(
            "PUT",
            "/api/domains/livekit.example.com/bind",
            &admin,
            Some(json!({ "app_id": id, "service": "caddy", "port": 8088, "tls_mode": "passthrough", "tls_port": 7443 })),
        )
        .await;
    assert_eq!(
        (st, d["tls_mode"].as_str(), d["tls_port"].as_i64()),
        (StatusCode::OK, Some("passthrough"), Some(7443))
    );
    // host 網路 service → 主機閘道
    assert_eq!(
        env.routes.lookup_passthrough("livekit.example.com").as_deref(),
        Some("host.docker.internal:7443")
    );
    assert_eq!(
        env.routes.lookup("livekit.example.com").as_deref(),
        Some("http://host.docker.internal:8088")
    );

    // 一般 service → edge 網路 alias
    env.call(
        "PUT",
        "/api/domains/relay.example.com/bind",
        &admin,
        Some(json!({ "app_id": id, "service": "relay", "port": 80, "tls_mode": "passthrough", "tls_port": 443 })),
    )
    .await;
    let alias = caloud_core::model_upstream_host(&id, "relay");
    assert_eq!(env.routes.lookup_passthrough("relay.example.com"), Some(format!("{alias}:443")));

    // 改回一般轉發 → 直通移除
    env.call(
        "PUT",
        "/api/domains/relay.example.com/bind",
        &admin,
        Some(json!({ "app_id": id, "service": "relay", "port": 80 })),
    )
    .await;
    assert!(env.routes.lookup_passthrough("relay.example.com").is_none());
    assert_eq!(env.routes.lookup("relay.example.com"), Some(format!("http://{alias}:80")));
}

// FR-MCP-04：create_git_app 只給 Admin，bind_domain 支援 TLS 直通
#[tokio::test]
async fn mcp_git_app_and_passthrough_tools() {
    let env = Env::new().await;
    let (admin, member) = admin_and_member(&env).await;
    std::fs::write(env.dir.join("gitrepo/docker-compose.yml"), LIVEKIT_COMPOSE).unwrap();
    let admin_token = env.create_token(&admin, "admin").await;
    let member_token = env.create_token(&member, "member").await;
    let names = |v: Value| {
        v["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert!(!names(env.rpc(&member_token, "tools/list", json!({})).await).contains(&"create_git_app".to_string()));
    let r = env
        .tool(
            &admin_token,
            "create_git_app",
            json!({ "git_url": "https://github.com/CAFECA-IO/x", "privileged": true, "services": ["caddy"], "env": "A=1" }),
        )
        .await;
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["env_keys"], json!(["A"]));
    let id = r["structuredContent"]["id"].as_str().unwrap().to_string();
    env.wait_app_status(&admin, &id, "running").await;
    let (_, d) = env
        .call("POST", "/api/domains", &admin, Some(json!({ "hostname": "lk.example.com" })))
        .await;
    env.mock.txt.lock().unwrap().insert(
        "_caloud-challenge.lk.example.com".into(),
        d["verify_token"].as_str().unwrap().into(),
    );
    env.tool(&admin_token, "verify_domain", json!({ "hostname": "lk.example.com" }))
        .await;
    let r = env
        .tool(
            &admin_token,
            "bind_domain",
            json!({ "hostname": "lk.example.com", "app_id": id, "service": "caddy", "port": 8088, "tls_passthrough_port": 7443 }),
        )
        .await;
    assert_eq!(r["structuredContent"]["tls_mode"], "passthrough", "{r}");
    assert_eq!(
        env.routes.lookup_passthrough("lk.example.com").as_deref(),
        Some("host.docker.internal:7443")
    );
}

// ───────────────────────── 平臺後台（M4） ─────────────────────────

// FR-OPS-03、FR-ENV-07
#[tokio::test]
async fn admin_manages_domain_resources_and_ids() {
    let env = Env::new().await;
    let (admin, member) = admin_and_member(&env).await;

    // 非 admin 不可用
    let (st, _) = env.call("GET", "/api/admin/domain-pools", &member, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // 尚無網域池時無法產生 ID
    let (st, _) = env
        .call("POST", "/api/admin/subdomain-ids", &admin, Some(json!({ "kind": "production" })))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let (st, pool) = env
        .call(
            "POST",
            "/api/admin/domain-pools",
            &admin,
            Some(json!({ "root": "Caloud.TW.", "purposes": ["production", "staging", "verify"], "cloudflare_zone": "caloud.tw" })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{pool}");
    assert_eq!(pool["root"], "caloud.tw");
    assert_eq!(pool["id_lengths"], json!({ "production": 8, "staging": 12, "verify": 16 }));

    // 同一環境用途只能屬於一個網域池；長度須不同；用途須合法
    let (st, _) = env
        .call(
            "POST",
            "/api/admin/domain-pools",
            &admin,
            Some(json!({ "root": "caloud.app", "purposes": ["staging"] })),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, _) = env
        .call(
            "POST",
            "/api/admin/domain-pools",
            &admin,
            Some(json!({ "root": "cafeca.io", "purposes": ["system"], "id_lengths": { "production": 8, "staging": 8, "verify": 16 } })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = env
        .call(
            "POST",
            "/api/admin/domain-pools",
            &admin,
            Some(json!({ "root": "cafeca.io", "purposes": ["mail"] })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = env
        .call(
            "POST",
            "/api/admin/domain-pools",
            &admin,
            Some(json!({ "root": "cafeca.io", "purposes": ["system"] })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);

    // 產生的 ID 符合規則（FR-ENV-07）
    let (_, reserved) = env.call("GET", "/api/admin/reserved-names", &admin, None).await;
    let reserved: Vec<String> = reserved
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    assert!(reserved.contains(&"www".to_string()));
    let rules = caloud_core::subdomain::IdRules {
        lengths: Default::default(),
        reserved,
    };
    for (kind, len) in [("production", 8), ("staging", 12), ("verify", 16)] {
        for _ in 0..20 {
            let (st, v) = env
                .call("POST", "/api/admin/subdomain-ids", &admin, Some(json!({ "kind": kind })))
                .await;
            assert_eq!(st, StatusCode::OK);
            let id = v["id"].as_str().unwrap();
            assert_eq!(id.len(), len);
            assert_eq!(v["hostname"], format!("{id}.caloud.tw"));
            let k = caloud_core::subdomain::EnvKind::parse(kind).unwrap();
            assert_eq!(caloud_core::subdomain::validate_id(id, k, &rules), Ok(()));
        }
    }
    let (st, _) = env
        .call("POST", "/api/admin/subdomain-ids", &admin, Some(json!({ "kind": "prod" })))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // 保留字
    let (st, _) = env
        .call("POST", "/api/admin/reserved-names", &admin, Some(json!({ "name": "Support" })))
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, _) = env
        .call("POST", "/api/admin/reserved-names", &admin, Some(json!({ "name": "support" })))
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, _) = env
        .call("POST", "/api/admin/reserved-names", &admin, Some(json!({ "name": "no spaces" })))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = env.call("DELETE", "/api/admin/reserved-names/support", &admin, None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);

    // 封鎖清單
    let (st, b) = env
        .call(
            "POST",
            "/api/admin/blocked-hosts",
            &admin,
            Some(json!({ "hostname": "Q7V2M4KX.caloud.tw", "reason": "檢舉：仿冒登入頁" })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(b["hostname"], "q7v2m4kx.caloud.tw");
    let (st, _) = env
        .call(
            "POST",
            "/api/admin/blocked-hosts",
            &admin,
            Some(json!({ "hostname": "x.caloud.tw", "reason": "" })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (_, list) = env.call("GET", "/api/admin/blocked-hosts", &admin, None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (st, _) = env
        .call("DELETE", "/api/admin/blocked-hosts/q7v2m4kx.caloud.tw", &admin, None)
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);

    // 修改與刪除網域池
    let (st, p) = env
        .call(
            "PUT",
            "/api/admin/domain-pools/caloud.tw",
            &admin,
            Some(
                json!({ "purposes": ["production", "staging", "verify"], "id_lengths": { "production": 10, "staging": 12, "verify": 16 } }),
            ),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(p["id_lengths"]["production"], 10);
    let (_, v) = env
        .call("POST", "/api/admin/subdomain-ids", &admin, Some(json!({ "kind": "production" })))
        .await;
    assert_eq!(v["id"].as_str().unwrap().len(), 10);
    let (st, _) = env.call("DELETE", "/api/admin/domain-pools/caloud.tw", &admin, None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = env.call("DELETE", "/api/admin/domain-pools/caloud.tw", &admin, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // 稽核
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action LIKE 'domain_pool_%' OR action LIKE 'host_%'")
        .fetch_one(&env.db)
        .await
        .unwrap();
    assert!(n >= 5);
}

/// 以原始 WebSocket 送出第一則訊息，回傳控制面的第一則回應
async fn raw_agent(env: &Env, first: Value) -> Value {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let url = format!("{}/agent/v1/connect", env.base.replacen("http://", "ws://", 1));
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    ws.send(Message::Text(first.to_string().into())).await.unwrap();
    loop {
        match ws.next().await.unwrap().unwrap() {
            Message::Text(t) => return serde_json::from_str(&t).unwrap(),
            _ => continue,
        }
    }
}

fn test_specs() -> Value {
    json!({ "cpu_cores": 8, "arch": "x86_64", "memory_bytes": 34359738368u64, "disk_free_bytes": 1, "os": "test",
            "gvisor": true, "buildkit": true, "agent_version": "0.1.0" })
}

async fn wait_node(env: &Env, admin: &str, name: &str, want: &str) -> Value {
    for _ in 0..100 {
        let (_, nodes) = env.call("GET", "/api/admin/nodes", admin, None).await;
        if let Some(n) = nodes.as_array().unwrap().iter().find(|n| n["name"] == name && n["status"] == want) {
            return n.clone();
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    panic!("node {name} never reached {want}");
}

// FR-OPS-04、FR-OPS-06
#[tokio::test]
async fn runner_agent_joins_once_heartbeats_and_can_be_drained() {
    use caloud_runner::client::{run_once, Options, Outcome};
    let env = Env::new().await;
    let (admin, member) = admin_and_member(&env).await;

    let (st, _) = env.call("POST", "/api/admin/nodes/join-tokens", &member, Some(json!({}))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = env
        .call(
            "POST",
            "/api/admin/nodes/join-tokens",
            &admin,
            Some(json!({ "expires_in_minutes": 90 })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, jt) = env.call("POST", "/api/admin/nodes/join-tokens", &admin, Some(json!({}))).await;
    assert_eq!(st, StatusCode::CREATED);
    let join = jt["token"].as_str().unwrap().to_string();
    assert!(join.starts_with("caloud_join_"));

    // 真正的 caloud-agent 以加入權杖註冊
    let opts = Options {
        server: env.base.clone(),
        join_token: Some(join.clone()),
        name: "runner-test-1".into(),
        state_dir: env.dir.join("agent1"),
    };
    let o = opts.clone();
    let agent = tokio::spawn(async move { run_once(&o).await.unwrap() });
    let node = wait_node(&env, &admin, "runner-test-1", "active").await;
    let id = node["id"].as_str().unwrap().to_string();
    assert!(node["owner"].is_null());
    assert!(node["specs"]["cpu_cores"].as_u64().unwrap() >= 1);
    // 可配置量由控制面以偵測規格保留 10% 計算（FR-OPS-08）
    let cores = node["specs"]["cpu_cores"].as_u64().unwrap();
    assert_eq!(node["allocatable"]["cpu_millis"].as_u64().unwrap(), cores * 900);

    // 節點權杖存在本機，資料庫只存雜湊
    let saved = caloud_runner::state::load(&opts.state_dir).unwrap().unwrap();
    assert_eq!(saved.node_id, id);
    assert!(saved.node_token.starts_with("caloud_node_"));
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nodes WHERE token_hash = ?")
        .bind(&saved.node_token)
        .fetch_one(&env.db)
        .await
        .unwrap();
    assert_eq!(n, 0);

    // 心跳更新用量
    for _ in 0..100 {
        let (_, nodes) = env.call("GET", "/api/admin/nodes", &admin, None).await;
        if nodes[0]["used"]["memory_bytes"].as_u64().unwrap_or(0) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let (_, nodes) = env.call("GET", "/api/admin/nodes", &admin, None).await;
    assert!(nodes[0]["used"]["memory_bytes"].as_u64().unwrap() > 0);

    // 加入權杖只能用一次；格式錯誤的第一則訊息被拒
    let r = raw_agent(
        &env,
        json!({ "type": "register", "join_token": join, "name": "evil", "specs": test_specs() }),
    )
    .await;
    assert_eq!(r["type"], "error");
    let r = raw_agent(&env, json!({ "type": "heartbeat", "used": { "cpu_millis": 0, "memory_bytes": 0 } })).await;
    assert_eq!(r["type"], "error");
    let r = raw_agent(
        &env,
        json!({ "type": "register", "node_token": "caloud_node_nope", "name": "x", "specs": test_specs() }),
    )
    .await;
    assert_eq!(r["type"], "error");

    // 運作中的節點不可直接移除；排空、恢復、再排空後移除
    let (st, _) = env.call("DELETE", &format!("/api/admin/nodes/{id}"), &admin, None).await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, n) = env.call("POST", &format!("/api/admin/nodes/{id}/drain"), &admin, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(n["status"], "draining");
    let (st, n) = env.call("POST", &format!("/api/admin/nodes/{id}/activate"), &admin, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(n["status"], "active");
    env.call("POST", &format!("/api/admin/nodes/{id}/drain"), &admin, None).await;
    let (st, _) = env.call("DELETE", &format!("/api/admin/nodes/{id}"), &admin, None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);

    // 移除後連線中斷，節點權杖失效，agent 不再重連
    let out = tokio::time::timeout(Duration::from_secs(5), agent).await.unwrap().unwrap();
    assert!(matches!(out, Outcome::Rejected(_)), "{out:?}");
    let again = run_once(&opts).await.unwrap();
    assert!(matches!(again, Outcome::Rejected(_)), "{again:?}");

    // 稽核
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action IN ('node_join', 'node_drain', 'node_activate', 'node_delete')")
            .fetch_one(&env.db)
            .await
            .unwrap();
    assert_eq!(n, 5);
}

// FR-OPS-04：60 秒沒有心跳的節點標為離線；以節點權杖重連後恢復
#[tokio::test]
async fn silent_node_goes_offline_and_returns_on_reconnect() {
    use caloud_runner::client::{run_once, Options};
    let env = Env::new().await;
    let (admin, _) = admin_and_member(&env).await;
    let (_, jt) = env.call("POST", "/api/admin/nodes/join-tokens", &admin, Some(json!({}))).await;
    let opts = Options {
        server: env.base.clone(),
        join_token: Some(jt["token"].as_str().unwrap().to_string()),
        name: "runner-test-2".into(),
        state_dir: env.dir.join("agent2"),
    };
    let o = opts.clone();
    let agent = tokio::spawn(async move { run_once(&o).await });
    let node = wait_node(&env, &admin, "runner-test-2", "active").await;
    let id = node["id"].as_str().unwrap().to_string();

    // 中斷 agent，並把最後心跳時間往前推
    agent.abort();
    let _ = agent.await;
    sqlx::query("UPDATE nodes SET last_seen = last_seen - 120 WHERE id = ?")
        .bind(&id)
        .execute(&env.db)
        .await
        .unwrap();
    let n = caloud_core::db::mark_offline_nodes(&env.db, caloud_core::now()).await.unwrap();
    assert_eq!(n, 1);
    wait_node(&env, &admin, "runner-test-2", "offline").await;
    let (st, _) = env.call("POST", &format!("/api/admin/nodes/{id}/activate"), &admin, None).await;
    assert_eq!(st, StatusCode::CONFLICT);

    // 以保存的節點權杖重連（不需要加入權杖）
    let o = Options { join_token: None, ..opts };
    let agent = tokio::spawn(async move { run_once(&o).await });
    let back = wait_node(&env, &admin, "runner-test-2", "active").await;
    assert_eq!(back["id"], id);
    agent.abort();
}

// ───────────────────────── M4-07：Runner 通道與 Upgrade 轉發 ─────────────────────────

/// 節點上的 App 替身：回 Host、回聲 body、WebSocket 回聲
async fn spawn_echo_app() -> std::net::SocketAddr {
    use axum::extract::ws::{Message as WsMsg, WebSocketUpgrade};
    use axum::http::HeaderMap;
    let app = Router::new()
        .route(
            "/hello",
            get(|h: HeaderMap| async move {
                let host = h.get("host").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
                let fwd = h
                    .get("x-forwarded-host")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                format!("hi {host} {fwd}")
            }),
        )
        .route("/echo", post(|body: axum::body::Bytes| async move { body }))
        .route(
            "/ws",
            get(|ws: WebSocketUpgrade| async move {
                ws.on_upgrade(|mut sock| async move {
                    while let Some(Ok(m)) = sock.recv().await {
                        match m {
                            WsMsg::Text(_) | WsMsg::Binary(_) => {
                                if sock.send(m).await.is_err() {
                                    break;
                                }
                            }
                            WsMsg::Close(_) => break,
                            _ => {}
                        }
                    }
                })
            }),
        );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    addr
}

/// 經 Edge 以指定 Host 開 WebSocket，送三則訊息確認回聲
async fn ws_echo_through(edge: std::net::SocketAddr, host: &str) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;
    let mut req = format!("ws://{edge}/ws").into_client_request().unwrap();
    req.headers_mut().insert("host", host.parse().unwrap());
    let (mut ws, res) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(res.status(), 101);
    for i in 0..3 {
        let text = format!("ping {i} via {host}");
        ws.send(Message::Text(text.clone().into())).await.unwrap();
        let back = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(back.into_text().unwrap().as_str(), text);
    }
    let big = vec![7u8; 200 * 1024];
    ws.send(Message::Binary(big.clone().into())).await.unwrap();
    let back = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(back.into_data().as_ref(), big.as_slice());
    ws.close(None).await.unwrap();
}

// FR-OPS-09、FR-DOM-06：Edge 經 Runner 通道轉發 HTTP 與 WebSocket；節點移除後回 502
#[tokio::test]
async fn edge_forwards_through_runner_tunnel_and_upgrades() {
    use caloud_core::model::Route;
    use caloud_runner::client::{run_once, Options, Outcome};
    let env = Env::new().await;
    let (admin, _) = admin_and_member(&env).await;

    // 權杖無效時不升級
    let r = env
        .http
        .get(env.url("/agent/v1/tunnel"))
        .header("authorization", "Bearer caloud_node_nope")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    let (_, jt) = env.call("POST", "/api/admin/nodes/join-tokens", &admin, Some(json!({}))).await;
    let opts = Options {
        server: env.base.clone(),
        join_token: Some(jt["token"].as_str().unwrap().to_string()),
        name: "runner-tunnel".into(),
        state_dir: env.dir.join("agent-tunnel"),
    };
    let o = opts.clone();
    let agent = tokio::spawn(async move { run_once(&o).await.unwrap() });
    let node = wait_node(&env, &admin, "runner-tunnel", "active").await;
    let id = node["id"].as_str().unwrap().to_string();
    for _ in 0..200 {
        if env.tunnels.contains(&id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(env.tunnels.contains(&id));

    let app = spawn_echo_app().await;
    let routes = RouteTable::default();
    let route = |host: &str, upstream: String, node: Option<String>| Route {
        hostname: host.into(),
        upstream,
        passthrough: None,
        node,
    };
    routes.replace(vec![
        route("tun.test", format!("http://{app}"), Some(id.clone())),
        route("direct.test", format!("http://{app}"), None),
        route("meta.test", "http://169.254.169.254:80".into(), Some(id.clone())),
    ]);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let edge = l.local_addr().unwrap();
    let tunnels = env.tunnels.clone();
    tokio::spawn(async move { caloud_proxy::serve_listener(l, routes, tunnels).await });
    let get = |host: &'static str, path: &'static str| env.http.get(format!("http://{edge}{path}")).header("host", host).send();

    // HTTP 經通道：Host 與 X-Forwarded-Host 保留
    let r = get("tun.test", "/hello").await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.text().await.unwrap(), "hi tun.test tun.test");
    let body = vec![b'x'; 300 * 1024];
    let r = env
        .http
        .post(format!("http://{edge}/echo"))
        .header("host", "tun.test")
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.bytes().await.unwrap().as_ref(), body.as_slice());
    // 直接上游仍可用
    assert_eq!(
        get("direct.test", "/hello").await.unwrap().text().await.unwrap(),
        "hi direct.test direct.test"
    );

    // WebSocket：經通道與直接上游都能雙向轉接（FR-DOM-06）
    ws_echo_through(edge, "tun.test").await;
    ws_echo_through(edge, "direct.test").await;

    // agent 拒絕雲端 metadata
    assert_eq!(get("meta.test", "/").await.unwrap().status(), StatusCode::BAD_GATEWAY);
    assert_eq!(get("nope.test", "/").await.unwrap().status(), StatusCode::NOT_FOUND);

    // 節點移除：通道立即中斷，Edge 回 502
    env.call("POST", &format!("/api/admin/nodes/{id}/drain"), &admin, None).await;
    let (st, _) = env.call("DELETE", &format!("/api/admin/nodes/{id}"), &admin, None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert!(!env.tunnels.contains(&id));
    assert_eq!(get("tun.test", "/hello").await.unwrap().status(), StatusCode::BAD_GATEWAY);
    let out = tokio::time::timeout(Duration::from_secs(5), agent).await.unwrap().unwrap();
    assert!(matches!(out, Outcome::Rejected(_)), "{out:?}");
}
