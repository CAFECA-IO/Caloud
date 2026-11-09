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

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let origin = format!("http://{addr}");
        let cfg = Config {
            public_origin: origin.clone(),
            api_listen: addr.to_string(),
            http_listen: "127.0.0.1:0".into(),
            https_listen: "127.0.0.1:0".into(),
            cafeca_wallet: mock_base.clone(),
            cafeca_rpc: None,
            data_dir: dir.join("data"),
            acme_email: None,
            acme_staging: true,
            edge_network: "caloud-edge-test".into(),
            default_role: "pending".into(),
            docker_bin: docker.display().to_string(),
            doh_url: format!("{mock_base}/dns-query"),
        };
        let db = caloud_core::db::connect(&cfg.data_dir).await.unwrap();
        let routes = RouteTable::default();
        let state = AppState {
            verifier: Arc::new(CafecaVerifier::new(cfg.cafeca_wallet.clone(), None)),
            runner: ComposeRunner::new(cfg.data_dir.join("apps"), cfg.edge_network.clone(), cfg.docker_bin.clone()),
            routes: routes.clone(),
            db: db.clone(),
            http: reqwest::Client::new(),
            cfg: Arc::new(cfg),
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
    assert!(log.contains(&format!("compose -p {project} -f")), "{log}");
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
