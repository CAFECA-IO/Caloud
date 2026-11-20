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
        // 替身 Cloudflare API（ADR 0016）：權杖 cf-good 看得到 caloud.tw
        .route(
            "/cf/zones",
            get(|h: axum::http::HeaderMap, Query(q): Query<HashMap<String, String>>| async move {
                let auth = h.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default();
                if auth != "Bearer cf-good" {
                    return Json(json!({ "success": false, "errors": [{ "code": 1000, "message": "Invalid API Token" }] }));
                }
                let result = if q.get("name").map(String::as_str) == Some("caloud.tw") {
                    json!([{ "id": "zone-caloud", "name": "caloud.tw" }])
                } else {
                    json!([])
                };
                Json(json!({ "success": true, "errors": [], "result": result }))
            }),
        )
        // 替身 GitHub（ADR 0017）：App 5826601、安裝 42 屬於使用者 octo
        .route(
            "/gh/app",
            get(|h: axum::http::HeaderMap| async move {
                let auth = h.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default();
                if auth.starts_with("Bearer ") && auth.matches('.').count() == 2 {
                    (StatusCode::OK, Json(json!({ "id": 5826601, "slug": "caloud-test" })))
                } else {
                    (StatusCode::UNAUTHORIZED, Json(json!({ "message": "A JSON web token could not be decoded" })))
                }
            }),
        )
        .route(
            "/ghweb/login/oauth/access_token",
            post(|axum::extract::Form(f): axum::extract::Form<HashMap<String, String>>| async move {
                if f.get("code").map(String::as_str) == Some("good-code") && f.get("client_secret").map(String::as_str) == Some("cs-secret") {
                    Json(json!({ "access_token": "user-tok", "token_type": "bearer" }))
                } else {
                    Json(json!({ "error": "bad_verification_code", "error_description": "The code passed is incorrect or expired." }))
                }
            }),
        )
        .route(
            "/gh/user/installations",
            get(|h: axum::http::HeaderMap| async move {
                assert_eq!(h.get("authorization").unwrap(), "Bearer user-tok");
                Json(json!({ "total_count": 1, "installations": [ { "id": 42, "account": { "login": "CAFECA-IO", "type": "Organization" } } ] }))
            }),
        )
        .route(
            "/gh/app/installations/{id}/access_tokens",
            post(|axum::extract::Path(id): axum::extract::Path<i64>| async move {
                Json(json!({ "token": format!("inst-tok-{id}"), "expires_at": "2099-01-01T00:00:00Z" }))
            }),
        )
        .route(
            "/gh/installation/repositories",
            get(|h: axum::http::HeaderMap| async move {
                assert_eq!(h.get("authorization").unwrap(), "Bearer inst-tok-42");
                Json(json!({ "total_count": 2, "repositories": [
                    { "full_name": "CAFECA-IO/secret-app", "private": true, "default_branch": "main" },
                    { "full_name": "CAFECA-IO/docs", "private": false, "default_branch": "trunk" } ] }))
            }),
        )
        // TXT 寫入替身 DoH（同名多筆以換行分隔）
        .route(
            "/cf/zones/{zone}/dns_records",
            post(|State(s): State<MockState>, Json(b): Json<Value>| async move {
                let name = b["name"].as_str().unwrap_or_default().to_string();
                let value = b["content"].as_str().unwrap_or_default().to_string();
                let mut txt = s.txt.lock().unwrap();
                let e = txt.entry(name.clone()).or_default();
                if !e.is_empty() {
                    e.push('\n');
                }
                e.push_str(&value);
                Json(json!({ "success": true, "result": { "id": format!("{name}|{value}") } }))
            }),
        )
        .route(
            "/cf/zones/{zone}/dns_records/{id}",
            axum::routing::delete(
                |State(s): State<MockState>, axum::extract::Path((_, id)): axum::extract::Path<(String, String)>| async move {
                    let (name, value) = id.split_once('|').unwrap_or_default();
                    let mut txt = s.txt.lock().unwrap();
                    if let Some(e) = txt.get_mut(name) {
                        *e = e.lines().filter(|l| *l != value).collect::<Vec<_>>().join("\n");
                    }
                    Json(json!({ "success": true, "result": { "id": id } }))
                },
            ),
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
                    .map(|v| {
                        v.lines()
                            .map(|v| json!({ "name": name, "type": 16, "data": format!("\"{v}\"") }))
                            .collect()
                    })
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
    state: AppState,
}

impl Env {
    async fn new() -> Self {
        Self::with_master_key(true).await
    }

    async fn with_master_key(master_key: bool) -> Self {
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
                 *' logs '*) echo 'web-1 | ready';; *caloud-test-*) [ -f '{slow}' ] && sleep 5;; *' up '*) echo \"UPENV P=$3 S=${{SESSION_SECRET:-}} D=${{CALOUD_DB_PASSWORD:-}}\" >> '{log}';; esac\nexit 0\n",
                log = dir.join("docker.log").display(),
                fail = dir.join("docker.fail").display(),
                slow = dir.join("docker.slow").display()
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
                "#!/bin/sh\necho \"$* [auth=${{GIT_CONFIG_VALUE_0:-none}}]\" >> '{log}'\nsrc=$2\ncase \"$*\" in *missing*) echo 'repository not found' >&2; exit 128;; *' checkout '*) cp -R '{repo}/.' \"$src\";; \
                 *' rev-parse '*) echo 98b705d0123456789abcdef0123456789abcdef0;; *' ls-remote '*) printf 'ref: refs/heads/main\\tHEAD\\n';; esac\nexit 0\n",
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
            // Pebble 測試（acme_issues_certificates_with_pebble）時才設定
            acme_email: std::env::var("PEBBLE_DIRECTORY").ok().map(|_| "ops@caloud.test".into()),
            acme_staging: true,
            acme_directory: std::env::var("PEBBLE_DIRECTORY").ok(),
            acme_ca_file: std::env::var("PEBBLE_CA").ok().map(PathBuf::from),
            edge_network: "caloud-edge-test".into(),
            default_role: "pending".into(),
            docker_bin: docker.display().to_string(),
            doh_url: format!("{mock_base}/dns-query"),
            git_bin: git.display().to_string(),
            cloudflare_api: format!("{mock_base}/cf"),
            github_api: format!("{mock_base}/gh"),
            github_web: format!("{mock_base}/ghweb"),
            // 測試的服務 IP 不存在；健康檢查另以單元測試驗證
            deploy_health_secs: 0,
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
            certs: Default::default(),
            master_key: master_key.then(|| Arc::new(caloud_core::secret::MasterKey::from_bytes(&[9u8; 32]).unwrap())),
            github: Default::default(),
            limiter: Default::default(),
        };
        let app = caloud_api::router(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Env {
            base: origin.clone(),
            origin,
            mock,
            routes,
            db,
            dir,
            http: reqwest::Client::new(),
            tunnels,
            state,
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
    tokio::spawn(async move { caloud_proxy::serve_listener(l, routes, tunnels, Default::default()).await });
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

// ───────────────────────── M5：平臺專案（docs/06 §3.7） ─────────────────────────

/// 把測試儲存庫內容換成 `files`
fn set_repo(env: &Env, files: &[(&str, &str)]) {
    let repo = env.dir.join("gitrepo");
    let _ = std::fs::remove_dir_all(&repo);
    for (f, body) in files {
        let p = repo.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    std::fs::create_dir_all(&repo).unwrap();
}

async fn wait_project(env: &Env, cookie: &str, id: &str) -> Value {
    for _ in 0..100 {
        let (st, v) = env.call("GET", &format!("/api/projects/{id}"), cookie, None).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        if v["project"]["status"] != "analyzing" {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    panic!("project {id} still analyzing");
}

// FR-GH-04、FR-AN-03、FR-AN-04、FR-AN-06、FR-AN-10：匯入、解析、確認、重新解析
#[tokio::test]
async fn project_import_analysis_and_confirmation() {
    let env = Env::new().await;
    let (admin, member) = admin_and_member(&env).await;
    set_repo(
        &env,
        &[
            ("package.json", "{\n  \"scripts\": {\n    \"build\": \"next build\",\n    \"start\": \"next start -p 4000\"\n  },\n  \"dependencies\": { \"next\": \"15\" }\n}\n"),
            ("package-lock.json", "{}"),
            ("src/a.ts", "export const k = process.env.STRIPE_KEY;\n"),
        ],
    );

    // 網址檢查
    for bad in ["http://github.com/a/b", "https://user:pw@github.com/a/b", "git@github.com:a/b.git"] {
        let (st, _) = env.call("POST", "/api/projects", &member, Some(json!({ "repo_url": bad }))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (st, _) = env
        .call(
            "POST",
            "/api/projects",
            &member,
            Some(json!({ "repo_url": "https://github.com/a/b", "root": "../x" })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let (st, p) = env
        .call(
            "POST",
            "/api/projects",
            &member,
            Some(json!({ "repo_url": "https://github.com/CAFECA-IO/SwarmStorage" })),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{p}");
    assert_eq!(p["name"], "SwarmStorage");
    assert_eq!(p["status"], "analyzing");
    let id = p["id"].as_str().unwrap().to_string();

    let v = wait_project(&env, &member, &id).await;
    assert_eq!(v["project"]["status"], "needs_review", "{v}");
    assert_eq!(v["project"]["branch"], "main");
    let a = &v["analysis"];
    assert_eq!(a["commit"], "98b705d0123456789abcdef0123456789abcdef0");
    assert_eq!(a["git_ref"], "HEAD");
    assert_eq!(a["result"]["source"]["template"], "node/next");
    let web = &a["result"]["services"][0];
    assert_eq!(
        (web["name"].as_str(), web["port"].as_u64(), web["public"].as_bool()),
        (Some("web"), Some(4000), Some(true))
    );
    let port = a["result"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "port")
        .unwrap();
    assert_eq!(port["evidence"][0], json!({ "file": "package.json", "line": 4 }));
    assert!(a["result"]["env"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["name"] == "STRIPE_KEY" && e["provider"] == "user"));
    assert!(v["confirmed"].is_null());
    // git 以安全設定執行
    let git_log = std::fs::read_to_string(env.dir.join("git.log")).unwrap();
    assert!(git_log.contains("protocol.allow=never") && git_log.contains("core.symlinks=false"));

    // 其他 member 看不到也改不了；admin 可以
    let (other, o) = env.login(acct(0xc3)).await;
    env.call(
        "PUT",
        &format!("/api/users/{}", o["account"].as_str().unwrap()),
        &admin,
        Some(json!({ "role": "member" })),
    )
    .await;
    let (st, _) = env.call("GET", &format!("/api/projects/{id}"), &other, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (_, list) = env.call("GET", "/api/projects", &other, None).await;
    assert_eq!(list.as_array().unwrap().len(), 0);
    let (_, list) = env.call("GET", "/api/projects", &admin, None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);

    // 確認：對外服務必須存在
    let (st, _) = env
        .call(
            "PUT",
            &format!("/api/projects/{id}/analysis"),
            &member,
            Some(json!({ "public_service": "nope" })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, p) = env
        .call(
            "PUT",
            &format!("/api/projects/{id}/analysis"),
            &member,
            Some(json!({ "public_service": "web" })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{p}");
    assert_eq!(p["status"], "confirmed");
    let first_confirmed = p["confirmed_analysis"].as_str().unwrap().to_string();

    // 儲存庫變成只有 Dockerfile（沒有 EXPOSE）→ 重新解析：新增一筆，確認版不變
    set_repo(&env, &[("Dockerfile", "FROM alpine\n")]);
    let (st, _) = env.call("POST", &format!("/api/projects/{id}/analysis"), &member, None).await;
    assert_eq!(st, StatusCode::ACCEPTED);
    let v = wait_project(&env, &member, &id).await;
    assert_eq!(v["project"]["status"], "confirmed", "已有確認版本");
    assert_eq!(v["confirmed"]["id"], first_confirmed.as_str());
    assert_ne!(v["analysis"]["id"], first_confirmed.as_str());
    assert_eq!(v["analysis"]["result"]["needs_confirmation"], true);
    let lows: Vec<usize> = v["analysis"]["result"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(_, f)| f["confidence"] == "low")
        .map(|(i, _)| i)
        .collect();
    assert!(!lows.is_empty());

    // 低信心未確認 → 400
    let (st, e) = env
        .call(
            "PUT",
            &format!("/api/projects/{id}/analysis"),
            &member,
            Some(json!({ "public_service": "web" })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(e["error"].as_str().unwrap().contains("低信心"), "{e}");
    // 編輯版違反平臺政策 → 400
    let (st, _) = env
        .call(
            "PUT",
            &format!("/api/projects/{id}/analysis"),
            &member,
            Some(json!({ "public_service": "web", "acknowledged": lows, "compose": "services:\n  web:\n    build: .\n    network_mode: host\n" })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = env
        .call(
            "PUT",
            &format!("/api/projects/{id}/analysis"),
            &member,
            Some(json!({ "public_service": "web", "acknowledged": lows, "compose": "services:\n  web:\n    build: .\n    ports: ['80:8080']\n" })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "編輯版不接受 ports");
    // 合法的編輯版
    let edited = "services:\n  web:\n    build: .\n    expose: ['8080']\n";
    let (st, p) = env
        .call(
            "PUT",
            &format!("/api/projects/{id}/analysis"),
            &member,
            Some(json!({ "public_service": "web", "acknowledged": lows, "compose": edited })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{p}");
    let v = wait_project(&env, &member, &id).await;
    assert_eq!(v["confirmed"]["id"], v["analysis"]["id"]);
    assert_eq!(v["confirmed"]["compose"], edited);
    assert_eq!(v["confirmed"]["acknowledged"], json!(lows));

    // PATCH 根目錄 → 重新解析
    let (st, p) = env
        .call("PATCH", &format!("/api/projects/{id}"), &member, Some(json!({ "root": "app/" })))
        .await;
    assert_eq!(st, StatusCode::OK, "{p}");
    assert_eq!(p["root"], "app");
    let v = wait_project(&env, &member, &id).await;
    assert!(v["analysis"]["result"]["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e.as_str().unwrap().contains("根目錄")));

    // 刪除：原始碼一併刪除
    let src = env.dir.join("data/projects").join(&id);
    assert!(src.exists());
    let (st, _) = env.call("DELETE", &format!("/api/projects/{id}"), &member, None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert!(!src.exists());
    let (st, _) = env.call("GET", &format!("/api/projects/{id}"), &member, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // 稽核
    let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log WHERE action LIKE 'project_%' ORDER BY id")
        .fetch_all(&env.db)
        .await
        .unwrap();
    assert_eq!(
        actions,
        [
            "project_create",
            "project_confirm",
            "project_analyze",
            "project_confirm",
            "project_update",
            "project_delete"
        ]
    );
}

// FR-GH-04、FR-AN-10：取得失敗為 error；數量上限
#[tokio::test]
async fn project_fetch_errors_and_quota() {
    let env = Env::new().await;
    let (_, member) = admin_and_member(&env).await;
    let (st, p) = env
        .call(
            "POST",
            "/api/projects",
            &member,
            Some(json!({ "repo_url": "https://github.com/a/missing", "git_ref": "dev" })),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    let v = wait_project(&env, &member, p["id"].as_str().unwrap()).await;
    assert_eq!(v["project"]["status"], "error");
    let err = v["project"]["error"].as_str().unwrap();
    assert!(err.contains("無法取得儲存庫") && err.contains("repository not found"), "{err}");
    assert!(!err.contains(&env.dir.display().to_string()), "不洩漏主機路徑");
    assert!(v["analysis"].is_null());
    let (st, _) = env
        .call(
            "PUT",
            &format!("/api/projects/{}/analysis", p["id"].as_str().unwrap()),
            &member,
            Some(json!({ "public_service": "web" })),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT);

    let account = format!("{:#x}", acct(0xb2));
    for i in 0..19 {
        sqlx::query("INSERT INTO projects (id, owner, name, repo_url, status, created_at, updated_at) VALUES (?, ?, 'x', 'https://x/y', 'error', 0, 0)")
            .bind(format!("filler-{i}"))
            .bind(&account)
            .execute(&env.db)
            .await
            .unwrap();
    }
    let (st, e) = env
        .call(
            "POST",
            "/api/projects",
            &member,
            Some(json!({ "repo_url": "https://github.com/a/b" })),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{e}");
}

// ───────────────────────── M4-09／M4-10：平臺環境（ADR 0015） ─────────────────────────

async fn wait_env(env: &Env, admin: &str, sub: &str, want: Option<&str>) -> Option<Value> {
    for _ in 0..200 {
        let (_, list) = env.call("GET", "/api/admin/environments", admin, None).await;
        let found = list.as_array().unwrap().iter().find(|e| e["subdomain"] == sub).cloned();
        match (&found, want) {
            (None, None) => return None,
            (Some(e), Some(w)) if e["status"] == w => return found,
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("environment {sub} never reached {want:?}");
}

// FR-OPS-12、FR-OPS-13：開出環境、經 agent 部署、路由帶節點、刪除
#[tokio::test]
async fn admin_opens_environment_on_isolated_runner() {
    use caloud_runner::client::{run_once, Options};
    use caloud_runner::envs::EnvRunner;
    use caloud_runner::selfcheck::SelfCheck;
    let env = Env::new().await;
    let (admin, member) = admin_and_member(&env).await;
    env.call(
        "POST",
        "/api/admin/domain-pools",
        &admin,
        Some(json!({ "root": "caloud.tw", "purposes": ["production", "staging", "verify"] })),
    )
    .await;
    let compose = "services:\n  web:\n    image: nginx:1.27\n  cache:\n    image: redis:7\n";
    let body = json!({ "kind": "verify", "compose_yaml": compose, "public_service": "web", "public_port": 80 });

    // 沒有節點
    let (st, _) = env.call("POST", "/api/admin/environments", &admin, Some(body.clone())).await;
    assert_eq!(st, StatusCode::CONFLICT);

    let docker = env.dir.join("fake-docker.sh").display().to_string();
    let start = |name: &'static str, check: SelfCheck| {
        let env_ref = &env;
        let docker = docker.clone();
        let admin = admin.clone();
        async move {
            let (_, jt) = env_ref.call("POST", "/api/admin/nodes/join-tokens", &admin, Some(json!({}))).await;
            let opts = Options {
                server: env_ref.base.clone(),
                join_token: Some(jt["token"].as_str().unwrap().to_string()),
                name: name.into(),
                state_dir: env_ref.dir.join(name),
                envs: EnvRunner::new(docker, env_ref.dir.join(name), vec!["1.1.1.1".into()]),
                check,
            };
            let agent = tokio::spawn(async move { run_once(&opts).await });
            let node = wait_node(env_ref, &admin, name, "active").await;
            (agent, node)
        }
    };

    // 未通過自我檢查的節點不接環境
    let (plain, node) = start(
        "runner-plain",
        SelfCheck {
            gvisor: true,
            isolation: false,
            buildkit: false,
            reason: Some("防火牆不完整".into()),
        },
    )
    .await;
    assert_eq!(node["specs"]["isolation"], false);
    assert_eq!(node["specs"]["self_check"], "防火牆不完整");
    let (st, e) = env.call("POST", "/api/admin/environments", &admin, Some(body.clone())).await;
    assert_eq!(st, StatusCode::CONFLICT, "{e}");

    let (agent, node) = start(
        "runner-iso",
        SelfCheck {
            gvisor: true,
            isolation: true,
            buildkit: false,
            reason: None,
        },
    )
    .await;
    let node_id = node["id"].as_str().unwrap().to_string();
    assert_eq!(node["specs"]["isolation"], true);

    // 權限與輸入檢查
    let (st, _) = env.call("POST", "/api/admin/environments", &member, Some(body.clone())).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    for bad in [
        json!({ "kind": "verify", "compose_yaml": "services:\n  web:\n    build: .\n", "public_service": "web", "public_port": 80 }),
        json!({ "kind": "verify", "compose_yaml": "services:\n  web:\n    image: x\n    privileged: true\n", "public_service": "web", "public_port": 80 }),
        json!({ "kind": "verify", "compose_yaml": compose, "public_service": "nope", "public_port": 80 }),
        json!({ "kind": "dev", "compose_yaml": compose, "public_service": "web", "public_port": 80 }),
        json!({ "kind": "verify", "compose_yaml": compose, "public_service": "web", "public_port": 0 }),
    ] {
        let (st, _) = env.call("POST", "/api/admin/environments", &admin, Some(bad.clone())).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}");
    }

    let (st, e1) = env.call("POST", "/api/admin/environments", &admin, Some(body.clone())).await;
    assert_eq!(st, StatusCode::CREATED, "{e1}");
    let sub = e1["subdomain"].as_str().unwrap().to_string();
    assert_eq!(sub.len(), 16, "verify 環境 16 字元");
    assert_eq!(e1["hostname"], format!("{sub}.caloud.tw"));
    assert_eq!(e1["node"], node_id.as_str());
    assert_eq!(e1["subnet_index"], 0);
    let running = wait_env(&env, &admin, &sub, Some("running")).await.unwrap();
    assert_eq!(
        running["services"],
        json!([{ "name": "web", "ip": "10.240.0.10" }, { "name": "cache", "ip": "10.240.0.11" }])
    );

    // Edge 路由：經該節點通道到對外服務的固定 IP
    env.routes.reload(&env.db).await.unwrap();
    let up = env.routes.lookup_route(&format!("{sub}.caloud.tw")).expect("route");
    assert_eq!(up.target, "http://10.240.0.10:80");
    assert_eq!(up.node.as_deref(), Some(node_id.as_str()));
    // agent 以 gVisor override 執行 docker compose
    let override_file = env.dir.join(format!("runner-iso/envs/{sub}/caloud.override.yml"));
    assert!(std::fs::read_to_string(override_file).unwrap().contains("runtime: runsc"));
    assert!(env.docker_log().contains(&format!("compose -p caloud-env-{sub}")));

    // 第二個環境用下一個網段；production 為 8 字元
    let mut b2 = body.clone();
    b2["kind"] = json!("production");
    let (_, e2) = env.call("POST", "/api/admin/environments", &admin, Some(b2)).await;
    assert_eq!(e2["subnet_index"], 1);
    assert_eq!(e2["subdomain"].as_str().unwrap().len(), 8);
    let (_, nodes) = env.call("GET", "/api/admin/nodes", &admin, None).await;
    let n = nodes.as_array().unwrap().iter().find(|n| n["id"] == node_id.as_str()).unwrap();
    assert_eq!(n["allocated"]["cpu_millis"], 1000, "每個服務保留 250m");

    // 刪除：agent 清除後紀錄與路由消失
    let (st, _) = env.call("DELETE", &format!("/api/admin/environments/{sub}"), &admin, None).await;
    assert_eq!(st, StatusCode::ACCEPTED);
    assert!(wait_env(&env, &admin, &sub, None).await.is_none());
    assert!(env.routes.lookup_route(&format!("{sub}.caloud.tw")).is_none());
    assert!(env.docker_log().contains("down -v --remove-orphans"));
    let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log WHERE action LIKE 'env_%' ORDER BY id")
        .fetch_all(&env.db)
        .await
        .unwrap();
    assert_eq!(actions, ["env_create", "env_create", "env_delete"]);
    agent.abort();
    plain.abort();
}

/// 驗證的煙霧測試目標（ADR 0020）：回 200
async fn smoke_server() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut c, _)) = l.accept().await {
            let mut buf = [0u8; 512];
            let _ = c.read(&mut buf).await;
            let _ = c.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok").await;
        }
    });
    addr
}

async fn wait_deploy(env: &Env, cookie: &str, id: &str, done: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..300 {
        let (_, v) = env.call("GET", &format!("/api/projects/{id}"), cookie, None).await;
        if done(&v["deployment"]) {
            return v["deployment"].clone();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let (_, v) = env.call("GET", &format!("/api/projects/{id}"), cookie, None).await;
    panic!("deployment never finished: {}", v["deployment"]);
}

// FR-ENV-12（ADR 0018）：確認後部署 → agent 取得原始碼、gVisor BuildKit 建置、啟動 → 健康檢查 → 網址
#[tokio::test]
#[allow(clippy::async_yields_async)] // 回傳 agent 的 JoinHandle，測試結束時 abort
async fn project_deploys_to_production_through_runner() {
    use caloud_runner::client::{run_once, Options};
    use caloud_runner::envs::EnvRunner;
    use caloud_runner::selfcheck::SelfCheck;
    let env = Env::new().await;
    let (admin, member) = admin_and_member(&env).await;
    env.call(
        "POST",
        "/api/admin/domain-pools",
        &admin,
        Some(json!({ "root": "caloud.tw", "purposes": ["production", "staging", "verify"] })),
    )
    .await;
    set_repo(
        &env,
        &[
            ("package.json", "{\n  \"scripts\": {\n    \"build\": \"next build\",\n    \"start\": \"next start -p 4000\"\n  },\n  \"dependencies\": { \"next\": \"15\" }\n}\n"),
            ("package-lock.json", "{}"),
        ],
    );
    let (_, p) = env
        .call(
            "POST",
            "/api/projects",
            &member,
            Some(json!({ "repo_url": "https://github.com/CAFECA-IO/SwarmStorage" })),
        )
        .await;
    let id = p["id"].as_str().unwrap().to_string();
    wait_project(&env, &member, &id).await;

    // 未確認 → 409
    let path = format!("/api/projects/{id}/deploy");
    let deploy = || env.call("POST", &path, &member, None);
    let (st, e) = deploy().await;
    assert_eq!(st, StatusCode::CONFLICT, "{e}");
    assert!(e["error"].as_str().unwrap().contains("確認"));
    let (st, _) = env
        .call(
            "PUT",
            &format!("/api/projects/{id}/analysis"),
            &member,
            Some(json!({ "public_service": "web" })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);

    // 沒有具備建置環境的節點 → 409
    let start = |name: &'static str, buildkit: bool| {
        let env_ref = &env;
        let admin = admin.clone();
        async move {
            let (_, jt) = env_ref.call("POST", "/api/admin/nodes/join-tokens", &admin, Some(json!({}))).await;
            let mut runner = EnvRunner::new(
                env_ref.dir.join("fake-docker.sh").display().to_string(),
                env_ref.dir.join(name),
                vec!["1.1.1.1".into()],
            );
            runner.git_bin = env_ref.dir.join("fake-git.sh").display().to_string();
            runner.smoke_target = Some(smoke_server().await);
            let opts = Options {
                server: env_ref.base.clone(),
                join_token: Some(jt["token"].as_str().unwrap().to_string()),
                name: name.into(),
                state_dir: env_ref.dir.join(name),
                envs: runner,
                check: SelfCheck {
                    gvisor: true,
                    isolation: true,
                    buildkit,
                    reason: None,
                },
            };
            let agent = tokio::spawn(async move { run_once(&opts).await });
            wait_node(env_ref, &admin, name, "active").await;
            agent
        }
    };
    let nobuild = start("runner-nobuild", false).await;
    let (st, e) = deploy().await;
    assert_eq!(st, StatusCode::CONFLICT, "{e}");
    assert!(e["error"].as_str().unwrap().contains("建置環境"), "{e}");
    let agent = start("runner-build", true).await;

    // ADR 0020：尚未驗證 → 先驗證，通過後部署
    let (st, d) = deploy().await;
    assert_eq!(st, StatusCode::ACCEPTED, "{d}");
    assert_eq!(d["verification"]["trigger"], "deploy", "{d}");
    assert_eq!(d["verification"]["deploy_after"], true);

    let d = wait_deploy(&env, &member, &id, |d| d["status"] == "running").await;
    let sub = d["subdomain"].as_str().unwrap().to_string();
    assert_eq!(sub.len(), 8, "production 8 字元");
    assert_eq!(d["url"], format!("https://{sub}.caloud.tw"));
    assert_eq!(d["commit"], "98b705d0123456789abcdef0123456789abcdef0");
    assert_eq!(d["stage"], "done");
    assert!(d["error"].is_null());
    let stages: Vec<&str> = d["logs"].as_array().unwrap().iter().map(|l| l["stage"].as_str().unwrap()).collect();
    for s in ["fetch", "build", "health"] {
        assert!(stages.contains(&s), "{s}: {}", d["logs"]);
    }
    assert!(d["logs"].to_string().contains("caloud-env/"), "{}", d["logs"]);
    let docker_log = env.docker_log();
    assert!(
        docker_log.contains(&format!(
            "run --rm --name caloud-build-{sub}-web --runtime runsc --network caloud-build"
        )),
        "{docker_log}"
    );
    let compose = std::fs::read_to_string(env.dir.join(format!("runner-build/envs/{sub}/compose.yml"))).unwrap();
    assert!(compose.contains(&format!("image: caloud-env/{sub}/web:98b705d01234")), "{compose}");
    // 路由：經節點通道到對外服務
    let up = env.routes.lookup_route(&format!("{sub}.caloud.tw")).expect("route");
    assert_eq!(up.target, "http://10.240.0.10:4000");

    // 重新部署沿用同一個子網域；建置失敗時舊版繼續執行、路由不變
    std::fs::write(env.dir.join("docker.fail"), "").unwrap();
    let (st, d2) = deploy().await;
    assert_eq!(st, StatusCode::ACCEPTED);
    assert_eq!(d2["subdomain"], sub.as_str());
    let d2 = wait_deploy(&env, &member, &id, |d| !d["error"].is_null()).await;
    assert_eq!(
        (d2["status"].as_str(), d2["stage"].as_str()),
        (Some("running"), Some("build")),
        "{d2}"
    );
    assert!(env.routes.lookup_route(&format!("{sub}.caloud.tw")).is_some());
    std::fs::remove_file(env.dir.join("docker.fail")).unwrap();
    deploy().await;
    wait_deploy(&env, &member, &id, |d| d["stage"] == "done").await;

    let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log WHERE action = 'project_deploy'")
        .fetch_all(&env.db)
        .await
        .unwrap();
    assert_eq!(actions.len(), 3);

    // 刪除專案：環境一併清除
    let (st, _) = env.call("DELETE", &format!("/api/projects/{id}"), &member, None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert!(wait_env(&env, &admin, &sub, None).await.is_none());
    assert!(env.routes.lookup_route(&format!("{sub}.caloud.tw")).is_none());
    agent.abort();
    nobuild.abort();
}

// FR-ENV-05（ADR 0019）：秘密變數加密保存、只寫不讀、部署時以行程環境變數注入；資料庫密碼由平臺產生且不變
#[tokio::test]
async fn project_vars_are_encrypted_and_injected() {
    use caloud_runner::client::{run_once, Options};
    use caloud_runner::envs::EnvRunner;
    use caloud_runner::selfcheck::SelfCheck;
    let env = Env::new().await;
    let (admin, member) = admin_and_member(&env).await;
    env.call(
        "POST",
        "/api/admin/domain-pools",
        &admin,
        Some(json!({ "root": "caloud.tw", "purposes": ["production", "staging", "verify"] })),
    )
    .await;
    set_repo(
        &env,
        &[
            (
                "package.json",
                "{\n  \"scripts\": { \"build\": \"next build\", \"start\": \"next start\" },\n  \"dependencies\": { \"next\": \"15\", \"@prisma/client\": \"6\" }\n}\n",
            ),
            ("prisma/schema.prisma", "datasource db {\n  provider = \"postgresql\"\n  url = env(\"PG_URL\")\n}\n"),
            ("src/a.ts", "const s = process.env.SESSION_SECRET;\nconst u = process.env.NEXT_PUBLIC_API_URL;\n"),
        ],
    );
    let (_, p) = env
        .call(
            "POST",
            "/api/projects",
            &member,
            Some(json!({ "repo_url": "https://github.com/CAFECA-IO/SwarmStorage" })),
        )
        .await;
    let id = p["id"].as_str().unwrap().to_string();
    let v = wait_project(&env, &member, &id).await;
    let lows: Vec<usize> = v["analysis"]["result"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(_, f)| f["confidence"] == "low")
        .map(|(i, _)| i)
        .collect();
    let (st, e) = env
        .call(
            "PUT",
            &format!("/api/projects/{id}/analysis"),
            &member,
            Some(json!({ "public_service": "web", "acknowledged": lows })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{e}");

    let vars = format!("/api/projects/{id}/vars");
    let (st, l) = env.call("GET", &vars, &member, None).await;
    assert_eq!(st, StatusCode::OK, "{l}");
    let item = |l: &Value, n: &str| l["items"].as_array().unwrap().iter().find(|i| i["name"] == n).cloned();
    let ss = item(&l, "SESSION_SECRET").expect("needed var");
    assert_eq!((ss["set"].as_bool(), ss["needed"].as_bool()), (Some(false), Some(true)));
    assert_eq!(item(&l, "NEXT_PUBLIC_API_URL").unwrap()["build_time"], true);

    // 權限與輸入檢查
    let (other, o) = env.login(acct(0xc7)).await;
    env.call(
        "PUT",
        &format!("/api/users/{}", o["account"].as_str().unwrap()),
        &admin,
        Some(json!({ "role": "member" })),
    )
    .await;
    let (st, _) = env.call("GET", &vars, &other, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    for bad in [
        json!({ "set": { "1BAD": "x" } }),
        json!({ "set": { "A-B": "x" } }),
        json!({ "set": { "CALOUD_DB_PASSWORD": "x" } }),
        json!({ "env": "dev", "set": { "A": "x" } }),
        json!({ "set": { "BIG": "x".repeat(33 * 1024) } }),
    ] {
        let (st, _) = env.call("PUT", &vars, &member, Some(bad.clone())).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (st, l) = env
        .call(
            "PUT",
            &vars,
            &member,
            Some(json!({ "set": { "SESSION_SECRET": "supersecret-value-42", "SHORT": "abc" } })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{l}");
    assert!(!l.to_string().contains("supersecret") && !l.to_string().contains("\"abc\""), "{l}");
    assert_eq!(item(&l, "SESSION_SECRET").unwrap()["hint"], "…e-42");
    assert!(item(&l, "SHORT").unwrap()["hint"].is_null(), "短值不給提示");
    assert_eq!(l["rev"], 1);
    let stored: Vec<String> = sqlx::query_scalar("SELECT value_enc FROM project_vars")
        .fetch_all(&env.db)
        .await
        .unwrap();
    assert!(stored.iter().all(|v| v.starts_with("v1:") && !v.contains("supersecret")));

    // Runner 與部署
    let (_, jt) = env.call("POST", "/api/admin/nodes/join-tokens", &admin, Some(json!({}))).await;
    let mut runner = EnvRunner::new(
        env.dir.join("fake-docker.sh").display().to_string(),
        env.dir.join("runner-vars"),
        vec!["1.1.1.1".into()],
    );
    runner.git_bin = env.dir.join("fake-git.sh").display().to_string();
    runner.smoke_target = Some(smoke_server().await);
    let opts = Options {
        server: env.base.clone(),
        join_token: Some(jt["token"].as_str().unwrap().to_string()),
        name: "runner-vars".into(),
        state_dir: env.dir.join("runner-vars"),
        envs: runner,
        check: SelfCheck {
            gvisor: true,
            isolation: true,
            buildkit: true,
            reason: None,
        },
    };
    let agent = tokio::spawn(async move { run_once(&opts).await });
    wait_node(&env, &admin, "runner-vars", "active").await;
    let path = format!("/api/projects/{id}/deploy");
    let (st, d) = env.call("POST", &path, &member, None).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{d}");
    assert!(d["verification"].is_object(), "先驗證：{d}");
    let d = wait_deploy(&env, &member, &id, |d| d["stage"] == "done").await;
    let sub = d["subdomain"].as_str().unwrap().to_string();
    assert_eq!(d["vars_pending"], false);
    assert_eq!(d["missing_vars"], json!(["NEXT_PUBLIC_API_URL"]));
    let upenv = |log: &str, project: &str| -> Vec<String> {
        log.lines()
            .filter(|l| l.starts_with(&format!("UPENV P=caloud-env-{project} ")))
            .map(|l| l.replacen(&format!("P=caloud-env-{project} "), "", 1))
            .collect()
    };
    let first = upenv(&env.docker_log(), &sub);
    assert_eq!(first.len(), 1, "{first:?}");
    let db_pw = first[0].split(" D=").nth(1).unwrap().to_string();
    assert!(
        first[0].starts_with("UPENV S=supersecret-value-42 D=") && db_pw.len() == 32,
        "{first:?}"
    );
    // 驗證環境：同樣的用戶變數、另外產生的資料庫密碼（ADR 0020）
    let verify_up: Vec<&str> = env
        .docker_log()
        .lines()
        .filter(|l| l.starts_with("UPENV") && !l.contains(&sub))
        .map(|_| "")
        .collect();
    assert_eq!(verify_up.len(), 1);
    assert!(env
        .docker_log()
        .lines()
        .any(|l| l.starts_with("UPENV") && !l.contains(&sub) && l.contains("S=supersecret-value-42") && !l.contains(&db_pw)));
    // override 只列名稱，只給建置的服務；檔案中沒有值
    let ov = std::fs::read_to_string(env.dir.join(format!("runner-vars/envs/{sub}/caloud.override.yml"))).unwrap();
    assert_eq!(ov.matches("environment:").count(), 1, "只有 web：{ov}");
    assert!(ov.contains("- SESSION_SECRET"), "{ov}");
    assert!(!ov.contains("supersecret") && !ov.contains(&db_pw));
    let (_, l) = env.call("GET", &vars, &member, None).await;
    let g = item(&l, "CALOUD_DB_PASSWORD").unwrap();
    assert_eq!((g["source"].as_str(), g["set"].as_bool()), (Some("generated"), Some(true)));
    assert!(g["hint"].is_null());

    // 變更後提示重新部署；資料庫密碼不變
    env.call(
        "PUT",
        &vars,
        &member,
        Some(json!({ "set": { "SESSION_SECRET": "rotated-secret-99" }, "unset": ["SHORT"] })),
    )
    .await;
    let (_, v) = env.call("GET", &format!("/api/projects/{id}"), &member, None).await;
    assert_eq!(v["deployment"]["vars_pending"], true);
    env.call("POST", &path, &member, None).await;
    let d = wait_deploy(&env, &member, &id, |d| d["stage"] == "done" && d["vars_pending"] == false).await;
    assert_eq!(d["vars_pending"], false);
    let second = upenv(&env.docker_log(), &sub);
    assert_eq!(second.last().unwrap(), &format!("UPENV S=rotated-secret-99 D={db_pw}"));

    // 稽核只有名稱
    let details: Vec<String> = sqlx::query_scalar("SELECT detail FROM audit_log WHERE action = 'project_vars' ORDER BY id")
        .fetch_all(&env.db)
        .await
        .unwrap();
    assert_eq!(
        details,
        [
            "via=web; production: set SESSION_SECRET,SHORT",
            "via=web; production: set SESSION_SECRET; unset SHORT"
        ]
    );
    // 刪除專案時一併刪除變數
    env.call("DELETE", &format!("/api/projects/{id}"), &member, None).await;
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_vars")
        .fetch_one(&env.db)
        .await
        .unwrap();
    assert_eq!(n, 0);
    agent.abort();
}

// ADR 0019：沒有主金鑰時拒絕保存
#[tokio::test]
async fn project_vars_require_master_key() {
    let env = Env::with_master_key(false).await;
    let (_, member) = admin_and_member(&env).await;
    set_repo(&env, &[("index.html", "<h1>x</h1>")]);
    let (_, p) = env
        .call(
            "POST",
            "/api/projects",
            &member,
            Some(json!({ "repo_url": "https://github.com/a/b" })),
        )
        .await;
    let id = p["id"].as_str().unwrap();
    wait_project(&env, &member, id).await;
    let (st, e) = env
        .call(
            "PUT",
            &format!("/api/projects/{id}/vars"),
            &member,
            Some(json!({ "set": { "A": "x" } })),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{e}");
    assert!(e["error"].as_str().unwrap().contains("CALOUD_MASTER_KEY"));
}

async fn wait_verification(env: &Env, cookie: &str, vid: &str) -> Value {
    for _ in 0..400 {
        let (_, v) = env.call("GET", &format!("/api/verifications/{vid}"), cookie, None).await;
        if !["queued", "running"].contains(&v["verification"]["status"].as_str().unwrap_or("running")) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("verification {vid} never finished");
}

// FR-CI-01～04、FR-RPT-01～03、FR-ENV-12（ADR 0020）：驗證、報告、與前次比較、取消、部署關卡、稽核來源
#[tokio::test]
#[allow(clippy::async_yields_async)]
async fn verification_reports_gate_and_compare() {
    use caloud_runner::client::{run_once, Options};
    use caloud_runner::envs::EnvRunner;
    use caloud_runner::selfcheck::SelfCheck;
    let env = Env::new().await;
    let (admin, member) = admin_and_member(&env).await;
    env.call(
        "POST",
        "/api/admin/domain-pools",
        &admin,
        Some(json!({ "root": "caloud.tw", "purposes": ["production", "staging", "verify"] })),
    )
    .await;
    let pkg = "{\n  \"scripts\": { \"build\": \"next build\", \"start\": \"next start\", \"test\": \"jest\" },\n  \"dependencies\": { \"next\": \"15\" }\n}\n";
    let leaked = format!("const k = 'AKIA{}';\n", "ABCDEFGHIJKLMNOP");
    set_repo(
        &env,
        &[("package.json", pkg), ("package-lock.json", "{}"), ("src/a.ts", leaked.as_str())],
    );
    let (_, p) = env
        .call(
            "POST",
            "/api/projects",
            &member,
            Some(json!({ "repo_url": "https://github.com/CAFECA-IO/SwarmStorage" })),
        )
        .await;
    let id = p["id"].as_str().unwrap().to_string();
    let v = wait_project(&env, &member, &id).await;
    assert_eq!(v["analysis"]["result"]["verify"]["test"]["command"], "npm run test");
    let verify = format!("/api/projects/{id}/verify");
    let (st, _) = env.call("POST", &verify, &member, None).await;
    assert_eq!(st, StatusCode::CONFLICT, "未確認不能驗證");
    env.call(
        "PUT",
        &format!("/api/projects/{id}/analysis"),
        &member,
        Some(json!({ "public_service": "web" })),
    )
    .await;

    let (_, jt) = env.call("POST", "/api/admin/nodes/join-tokens", &admin, Some(json!({}))).await;
    let mut runner = EnvRunner::new(
        env.dir.join("fake-docker.sh").display().to_string(),
        env.dir.join("runner-v"),
        vec!["1.1.1.1".into()],
    );
    runner.git_bin = env.dir.join("fake-git.sh").display().to_string();
    runner.smoke_target = Some(smoke_server().await);
    let opts = Options {
        server: env.base.clone(),
        join_token: Some(jt["token"].as_str().unwrap().to_string()),
        name: "runner-v".into(),
        state_dir: env.dir.join("runner-v"),
        envs: runner,
        check: SelfCheck {
            gvisor: true,
            isolation: true,
            buildkit: true,
            reason: None,
        },
    };
    let agent = tokio::spawn(async move { run_once(&opts).await });
    wait_node(&env, &admin, "runner-v", "active").await;

    // 1. 有秘密外洩 → 未通過
    let (st, v1) = env.call("POST", &verify, &member, None).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v1}");
    assert_eq!((v1["status"].as_str(), v1["trigger"].as_str()), (Some("running"), Some("manual")));
    let vid1 = v1["id"].as_str().unwrap().to_string();
    let d1 = wait_verification(&env, &member, &vid1).await;
    let r1 = &d1["verification"];
    assert_eq!(r1["status"], "failed", "{d1}");
    assert_eq!(r1["counts"]["high"], 1, "{d1}");
    for st in ["fetch", "analyze", "build", "start", "test", "smoke", "logs", "report", "destroy"] {
        assert_eq!(r1["stages"][st], "passed", "{st}: {}", r1["stages"]);
    }
    assert_eq!(r1["stages"]["static"], "failed");
    let issue = d1["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["rule"] == "secret/aws-access-key")
        .unwrap()
        .clone();
    assert_eq!(
        (issue["file"].as_str(), issue["line"].as_i64(), issue["change"].as_str()),
        (Some("src/a.ts"), Some(1), Some("new"))
    );
    assert!(!d1.to_string().contains("AKIAABCDEFGHIJKLMNOP"), "值遮蔽");
    assert!(
        d1["logs"].as_array().unwrap().iter().any(|l| l["stage"] == "test"),
        "{}",
        d1["logs"]
    );
    // 測試在 gVisor 沙箱執行；驗證環境已銷毀、紀錄刪除
    let log = env.docker_log();
    assert!(
        log.contains("--runtime runsc --network caloud-build") && log.contains("sh -c npm ci && npm run test"),
        "{log}"
    );
    let envs_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM environments WHERE kind = 'verify'")
        .fetch_one(&env.db)
        .await
        .unwrap();
    assert_eq!(envs_left, 0);

    // 報告：JSON、Markdown（附件）
    let r = env
        .http
        .get(env.url(&format!("/api/verifications/{vid1}/report?format=md")))
        .header("cookie", &member)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(r.headers()["content-disposition"].to_str().unwrap().contains(".md"));
    let md = r.text().await.unwrap();
    assert!(
        md.contains("# 驗證報告：SwarmStorage") && md.contains("**failed**") && md.contains("AWS"),
        "{md}"
    );
    let r = env
        .http
        .get(env.url(&format!("/api/verifications/{vid1}/report")))
        .header("cookie", &member)
        .send()
        .await
        .unwrap();
    assert!(r.headers()["content-disposition"].to_str().unwrap().contains(".json"));

    // 權限
    let (other, o) = env.login(acct(0xd1)).await;
    env.call(
        "PUT",
        &format!("/api/users/{}", o["account"].as_str().unwrap()),
        &admin,
        Some(json!({ "role": "member" })),
    )
    .await;
    let (st, _) = env.call("GET", &format!("/api/verifications/{vid1}"), &other, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = env.call("POST", &verify, &other, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // 2. 部署關卡：未通過 → 部署改為先驗證；member 不能強制；admin 可強制並留稽核
    let deploy = format!("/api/projects/{id}/deploy");
    let (_, g) = env.call("POST", &deploy, &member, None).await;
    let vid2 = g["verification"]["id"].as_str().unwrap().to_string();
    assert_eq!(wait_verification(&env, &member, &vid2).await["verification"]["status"], "failed");
    let (_, v) = env.call("GET", &format!("/api/projects/{id}"), &member, None).await;
    assert!(v["deployment"].is_null(), "未通過不部署");
    assert_eq!(v["verification"]["verified"], false);
    let (st, _) = env.call("POST", &deploy, &member, Some(json!({ "force": true }))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, d) = env.call("POST", &deploy, &admin, Some(json!({ "force": true }))).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{d}");
    assert!(d["subdomain"].is_string(), "{d}");
    wait_deploy(&env, &member, &id, |d| d["stage"] == "done").await;

    // 3. 修正後通過：與前次比較 → 已修正
    set_repo(
        &env,
        &[
            ("package.json", pkg),
            ("package-lock.json", "{}"),
            ("src/a.ts", "export const ok = 1;\n"),
        ],
    );
    let (_, v3) = env.call("POST", &verify, &member, None).await;
    let d3 = wait_verification(&env, &member, v3["id"].as_str().unwrap()).await;
    assert_eq!(d3["verification"]["status"], "passed", "{d3}");
    assert_eq!(d3["previous"], vid2.as_str());
    assert!(
        d3["fixed"].as_array().unwrap().iter().any(|i| i["rule"] == "secret/aws-access-key"),
        "{d3}"
    );
    let (_, v) = env.call("GET", &format!("/api/projects/{id}"), &member, None).await;
    assert_eq!(v["verification"]["verified"], true);
    let (_, list) = env.call("GET", "/api/projects", &member, None).await;
    assert_eq!(list[0]["verify_status"], "passed");
    // 已通過 → 直接部署
    let (_, d) = env.call("POST", &deploy, &member, None).await;
    assert!(d["subdomain"].is_string(), "{d}");
    wait_deploy(&env, &member, &id, |d| d["stage"] == "done").await;

    // 4. 同分支的新驗證取消舊的；手動取消
    std::fs::write(env.dir.join("docker.slow"), "").unwrap();
    let (_, a) = env.call("POST", &verify, &member, None).await;
    let (_, b) = env.call("POST", &verify, &member, None).await;
    let da = wait_verification(&env, &member, a["id"].as_str().unwrap()).await;
    assert_eq!(da["verification"]["status"], "canceled");
    assert_eq!(da["verification"]["error"], "同分支有新的驗證");
    let bid = b["id"].as_str().unwrap();
    let (st, _) = env.call("POST", &format!("/api/verifications/{bid}/cancel"), &member, None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert_eq!(wait_verification(&env, &member, bid).await["verification"]["status"], "canceled");
    let (st, _) = env.call("POST", &format!("/api/verifications/{bid}/cancel"), &member, None).await;
    assert_eq!(st, StatusCode::CONFLICT);
    std::fs::remove_file(env.dir.join("docker.slow")).unwrap();
    // 取消的環境由 agent 清除
    for _ in 0..200 {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM environments WHERE kind = 'verify'")
            .fetch_one(&env.db)
            .await
            .unwrap();
        if n == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM environments WHERE kind = 'verify'")
        .fetch_one(&env.db)
        .await
        .unwrap();
    assert_eq!(n, 0);

    // 稽核來源（FR-OPS-01）
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT action, detail FROM audit_log WHERE action IN ('project_verify', 'project_deploy') ORDER BY id")
            .fetch_all(&env.db)
            .await
            .unwrap();
    assert!(
        rows.iter().all(|(_, d)| d.as_deref().unwrap_or_default().starts_with("via=")),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|(a, d)| a == "project_deploy" && d.as_deref().unwrap_or_default().ends_with(" forced")),
        "{rows:?}"
    );
    agent.abort();
}

// ───────────────────────── Edge TLS 與憑證（ADR 0016） ─────────────────────────

// FR-OPS-03、FR-OPS-14：Cloudflare 權杖經驗證後加密保存，只回傳提示；FR-OPS-10、FR-DOM-05、FR-DOM-08：需要的憑證
#[tokio::test]
async fn admin_sets_cloudflare_token_and_certificates_follow_domains() {
    use caloud_api::certs::{wanted, Kind};
    let env = Env::new().await;
    let (admin, member) = admin_and_member(&env).await;
    let pool =
        |token: &str| json!({ "root": "caloud.tw", "purposes": ["verify"], "cloudflare_zone": "caloud.tw", "cloudflare_token": token });

    // 權杖無效、沒有 zone → 400；member → 403
    let (st, v) = env.call("POST", "/api/admin/domain-pools", &admin, Some(pool("cf-bad"))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["error"].as_str().unwrap_or_default().contains("Invalid API Token"), "{v}");
    let (st, _) = env
        .call(
            "POST",
            "/api/admin/domain-pools",
            &admin,
            Some(json!({ "root": "caloud.tw", "purposes": ["verify"], "cloudflare_token": "cf-good" })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = env.call("POST", "/api/admin/domain-pools", &member, Some(pool("cf-good"))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(env
        .call("GET", "/api/admin/domain-pools", &admin, None)
        .await
        .1
        .as_array()
        .unwrap()
        .is_empty());

    // 設定成功：回應只有提示；資料庫存密文
    let (st, p) = env.call("POST", "/api/admin/domain-pools", &admin, Some(pool("cf-good"))).await;
    assert_eq!(st, StatusCode::CREATED, "{p}");
    assert_eq!(p["cloudflare_token_hint"], "…good");
    assert!(!p.to_string().contains("cf-good"));
    let (enc, zone_id): (String, String) =
        sqlx::query_as("SELECT cloudflare_token_enc, cloudflare_zone_id FROM domain_pools WHERE root = 'caloud.tw'")
            .fetch_one(&env.db)
            .await
            .unwrap();
    assert!(enc.starts_with("v1:") && !enc.contains("cf-good"));
    assert_eq!(zone_id, "zone-caloud");
    let key = env.state.master_key.as_ref().unwrap();
    assert_eq!(key.open("cloudflare_token", "caloud.tw", &enc).unwrap(), b"cf-good");

    // 網域池 wildcard（DNS-01）
    let w = wanted(&env.state).await.unwrap();
    assert_eq!(w.len(), 1);
    assert_eq!((w[0].name.as_str(), w[0].kind), ("caloud.tw", Kind::Dns01));
    assert_eq!(w[0].names, vec!["caloud.tw", "*.caloud.tw"]);

    // 憑證列表：admin 可看、member 不行
    let (st, certs) = env.call("GET", "/api/admin/certificates", &admin, None).await;
    assert_eq!((st, certs), (StatusCode::OK, json!([])));
    assert_eq!(
        env.call("GET", "/api/admin/certificates", &member, None).await.0,
        StatusCode::FORBIDDEN
    );

    // 同一 App 綁多個網域（FR-DOM-08）：已驗證、綁定運作中 App 的網域需要 HTTP-01 憑證；wildcard 涵蓋的不另外申請
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
    for host in ["shop.example.com", "www.example.com", "docs.caloud.tw"] {
        let (_, d) = env.call("POST", "/api/domains", &admin, Some(json!({ "hostname": host }))).await;
        env.mock
            .txt
            .lock()
            .unwrap()
            .insert(format!("_caloud-challenge.{host}"), d["verify_token"].as_str().unwrap().to_string());
        assert_eq!(
            env.call("POST", &format!("/api/domains/{host}/verify"), &admin, None).await.0,
            StatusCode::OK
        );
        let (st, _) = env
            .call(
                "PUT",
                &format!("/api/domains/{host}/bind"),
                &admin,
                Some(json!({ "app_id": id, "service": "web", "port": 80 })),
            )
            .await;
        assert_eq!(st, StatusCode::OK);
        assert!(env.routes.lookup(host).is_some(), "{host}");
    }
    let names: Vec<(String, Kind)> = wanted(&env.state).await.unwrap().into_iter().map(|w| (w.name, w.kind)).collect();
    assert_eq!(
        names,
        vec![
            ("caloud.tw".into(), Kind::Dns01),
            ("shop.example.com".into(), Kind::Http01),
            ("www.example.com".into(), Kind::Http01),
        ]
    );

    // App 停止 → 不再需要 HTTP-01 憑證
    env.call("POST", &format!("/api/apps/{id}/stop"), &admin, None).await;
    assert_eq!(wanted(&env.state).await.unwrap().len(), 1);

    // 換 zone 必須重新輸入權杖；清除權杖後不再申請 wildcard
    let (st, _) = env
        .call(
            "PUT",
            "/api/admin/domain-pools/caloud.tw",
            &admin,
            Some(json!({ "purposes": ["verify"], "cloudflare_zone": "other.tw" })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, p) = env
        .call(
            "PUT",
            "/api/admin/domain-pools/caloud.tw",
            &admin,
            Some(json!({ "purposes": ["verify"], "cloudflare_zone": "caloud.tw" })),
        )
        .await;
    assert_eq!((st, &p["cloudflare_token_hint"]), (StatusCode::OK, &json!("…good")));
    let (st, p) = env
        .call(
            "PUT",
            "/api/admin/domain-pools/caloud.tw",
            &admin,
            Some(json!({ "purposes": ["verify"], "cloudflare_zone": "caloud.tw", "cloudflare_token": "" })),
        )
        .await;
    assert_eq!((st, &p["cloudflare_token_hint"]), (StatusCode::OK, &Value::Null));
    assert!(wanted(&env.state).await.unwrap().is_empty());
}

// FR-OPS-14：沒有主金鑰時拒絕保存權杖
#[tokio::test]
async fn cloudflare_token_requires_master_key() {
    let env = Env::with_master_key(false).await;
    let (admin, _) = env.login(acct(0xa1)).await;
    let (st, v) = env
        .call(
            "POST",
            "/api/admin/domain-pools",
            &admin,
            Some(json!({ "root": "caloud.tw", "purposes": ["verify"], "cloudflare_zone": "caloud.tw", "cloudflare_token": "cf-good" })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(v["error"].as_str().unwrap_or_default().contains("CALOUD_MASTER_KEY"), "{v}");
    // 不帶權杖仍可建立
    let (st, p) = env
        .call(
            "POST",
            "/api/admin/domain-pools",
            &admin,
            Some(json!({ "root": "caloud.tw", "purposes": ["verify"], "cloudflare_zone": "caloud.tw" })),
        )
        .await;
    assert_eq!((st, &p["cloudflare_token_hint"]), (StatusCode::CREATED, &Value::Null));
}

// FR-OPS-10、FR-DOM-05：以 Pebble 實際走一次 ACME（DNS-01 經 Cloudflare 替身、HTTP-01）。
// 需先啟動 Pebble（PEBBLE_VA_ALWAYS_VALID=1），並設定 PEBBLE_DIRECTORY、PEBBLE_CA：
//   PEBBLE_VA_ALWAYS_VALID=1 pebble -config pebble-config.json &
//   PEBBLE_DIRECTORY=https://localhost:14000/dir PEBBLE_CA=<pebble.minica.pem> cargo test -p caloud-api -- --ignored pebble
#[tokio::test]
#[ignore]
async fn acme_issues_certificates_with_pebble() {
    let env = Env::new().await;
    let (admin, _) = env.login(acct(0xa1)).await;
    let (st, _) = env
        .call(
            "POST",
            "/api/admin/domain-pools",
            &admin,
            Some(json!({ "root": "caloud.test", "purposes": ["verify"], "cloudflare_zone": "caloud.tw", "cloudflare_token": "cf-good" })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
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
        .call("POST", "/api/domains", &admin, Some(json!({ "hostname": "shop.example.test" })))
        .await;
    env.mock.txt.lock().unwrap().insert(
        "_caloud-challenge.shop.example.test".into(),
        d["verify_token"].as_str().unwrap().into(),
    );
    env.call("POST", "/api/domains/shop.example.test/verify", &admin, None).await;
    env.call(
        "PUT",
        "/api/domains/shop.example.test/bind",
        &admin,
        Some(json!({ "app_id": id, "service": "web", "port": 80 })),
    )
    .await;

    caloud_api::certs::scan(&env.state).await.unwrap();
    let (_, certs) = env.call("GET", "/api/admin/certificates", &admin, None).await;
    let certs = certs.as_array().unwrap();
    assert_eq!(certs.len(), 2, "{certs:?}");
    for c in certs {
        assert_eq!(c["status"], "issued", "{c}");
        assert!(c["not_after"].as_i64().unwrap() > caloud_core::now());
    }
    assert!(env.state.certs.resolve("abc.caloud.test").is_some());
    assert!(env.state.certs.resolve("caloud.test").is_some());
    assert!(env.state.certs.resolve("shop.example.test").is_some());
    let (_, doms) = env.call("GET", "/api/domains", &admin, None).await;
    assert_eq!(doms[0]["tls_status"], "issued");
    // ACME TXT 用完即刪；帳號金鑰與私鑰權限 0600
    assert!(env
        .mock
        .txt
        .lock()
        .unwrap()
        .get("_acme-challenge.caloud.test")
        .is_none_or(|v| v.is_empty()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let key = env.dir.join("data/certs/staging/caloud.test/key.pem");
        assert_eq!(std::fs::metadata(key).unwrap().permissions().mode() & 0o777, 0o600);
        let acct = env.dir.join("data/acme/staging/account.json");
        assert_eq!(std::fs::metadata(acct).unwrap().permissions().mode() & 0o777, 0o600);
    }

    // 已簽發且未近到期 → 不重簽
    let before = env.state.certs.resolve("abc.caloud.test").unwrap().not_after;
    caloud_api::certs::scan(&env.state).await.unwrap();
    assert_eq!(env.state.certs.resolve("abc.caloud.test").unwrap().not_after, before);
}

// ───────────────────────── GitHub App 與主控台防護（ADR 0017） ─────────────────────────

fn gh_sig(secret: &str, body: &str) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
    format!("sha256={}", hex::encode(ring::hmac::sign(&key, body.as_bytes()).as_ref()))
}

impl Env {
    async fn webhook(&self, event: &str, delivery: &str, body: &Value, secret: &str) -> StatusCode {
        let b = body.to_string();
        self.http
            .post(self.url("/api/github/webhook"))
            .header("x-github-event", event)
            .header("x-github-delivery", delivery)
            .header("x-hub-signature-256", gh_sig(secret, &b))
            .header("content-type", "application/json")
            .body(b)
            .send()
            .await
            .unwrap()
            .status()
    }
}

// FR-GH-05、FR-GH-01、FR-GH-02
#[tokio::test]
async fn github_app_install_private_import_and_webhook() {
    let env = Env::new().await;
    let (admin, member) = admin_and_member(&env).await;
    let pem = include_str!("fixtures/github-app-test.pem");
    let wh = "whsec-0123456789abcdef";

    // 未設定時無法安裝
    let (st, _) = env.call("GET", "/api/github/install", &member, None).await;
    assert_eq!(st, StatusCode::CONFLICT);
    // 後台設定：member 不行；首次缺機密 → 400；私鑰錯誤 → 400；成功只回提示
    let body =
        json!({ "app_id": 5826601, "client_id": "Iv23liTest", "client_secret": "cs-secret", "private_key": pem, "webhook_secret": wh });
    assert_eq!(
        env.call("PUT", "/api/admin/github-app", &member, Some(body.clone())).await.0,
        StatusCode::FORBIDDEN
    );
    let (st, _) = env
        .call(
            "PUT",
            "/api/admin/github-app",
            &admin,
            Some(json!({ "app_id": 5826601, "client_id": "Iv23liTest" })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let mut wrong = body.clone();
    wrong["app_id"] = json!(999);
    let (st, v) = env.call("PUT", "/api/admin/github-app", &admin, Some(wrong)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, info) = env.call("PUT", "/api/admin/github-app", &admin, Some(body)).await;
    assert_eq!(st, StatusCode::OK, "{info}");
    assert_eq!(info["slug"], "caloud-test");
    assert_eq!(info["webhook_secret_hint"], "…cdef");
    assert!(!info.to_string().contains("cs-secret") && !info.to_string().contains("PRIVATE KEY"));
    let enc: String = sqlx::query_scalar("SELECT private_key_enc FROM github_app")
        .fetch_one(&env.db)
        .await
        .unwrap();
    assert!(enc.starts_with("v1:") && !enc.contains("PRIVATE"));
    // 更新時省略機密：維持原值
    let (st, _) = env
        .call(
            "PUT",
            "/api/admin/github-app",
            &admin,
            Some(json!({ "app_id": 5826601, "client_id": "Iv23liTest" })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);

    // 安裝：state 一次性且綁定帳號
    let (_, v) = env.call("GET", "/api/github/install", &member, None).await;
    let url = v["url"].as_str().unwrap();
    assert!(url.contains("/ghweb/apps/caloud-test/installations/new?state="), "{url}");
    let state = url.split("state=").nth(1).unwrap().to_string();
    // 回呼只轉回主控台，不綁定
    let res = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(env.url(&format!(
            "/api/github/callback?code=good-code&installation_id=42&state={state}&setup_action=install"
        )))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        res.headers()["location"],
        format!("/#github-complete/code=good-code&installation_id=42&state={state}&setup_action=install").as_str()
    );
    // 別人不能用這個 state；用錯 code 失敗且 state 作廢
    let done = |code: &str, st: &str| json!({ "code": code, "installation_id": 42, "state": st });
    assert_eq!(
        env.call("POST", "/api/github/complete", &admin, Some(done("good-code", &state)))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        env.call("POST", "/api/github/complete", &member, Some(done("good-code", &state)))
            .await
            .0,
        StatusCode::BAD_REQUEST,
        "state 已作廢"
    );
    let (_, v) = env.call("GET", "/api/github/install", &member, None).await;
    let state = v["url"].as_str().unwrap().split("state=").nth(1).unwrap().to_string();
    let (st, v) = env
        .call("POST", "/api/github/complete", &member, Some(done("bad-code", &state)))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    // 安裝不屬於該 GitHub 使用者 → 403
    let (_, v) = env.call("GET", "/api/github/install", &member, None).await;
    let state = v["url"].as_str().unwrap().split("state=").nth(1).unwrap().to_string();
    let (st, _) = env
        .call(
            "POST",
            "/api/github/complete",
            &member,
            Some(json!({ "code": "good-code", "installation_id": 77, "state": state })),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // 成功
    let (_, v) = env.call("GET", "/api/github/install", &member, None).await;
    let state = v["url"].as_str().unwrap().split("state=").nth(1).unwrap().to_string();
    let (st, inst) = env
        .call("POST", "/api/github/complete", &member, Some(done("good-code", &state)))
        .await;
    assert_eq!(st, StatusCode::OK, "{inst}");
    assert_eq!(inst["account_login"], "CAFECA-IO");
    let (_, list) = env.call("GET", "/api/github/installations", &member, None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    // 已綁定他人的安裝 → 409
    let (_, v) = env.call("GET", "/api/github/install", &admin, None).await;
    let state = v["url"].as_str().unwrap().split("state=").nth(1).unwrap().to_string();
    assert_eq!(
        env.call("POST", "/api/github/complete", &admin, Some(done("good-code", &state)))
            .await
            .0,
        StatusCode::CONFLICT
    );

    // 儲存庫列表
    let (st, repos) = env.call("GET", "/api/github/installations/42/repositories", &member, None).await;
    assert_eq!(st, StatusCode::OK, "{repos}");
    assert_eq!(repos[0]["full_name"], "CAFECA-IO/docs");
    assert_eq!(repos[1]["private"], true);

    // 以安裝匯入私有儲存庫：權杖只經環境變數給 git
    std::fs::write(env.dir.join("gitrepo/index.html"), "<h1>hi</h1>").unwrap();
    let (st, p) = env
        .call(
            "POST",
            "/api/projects",
            &member,
            Some(json!({ "installation_id": 42, "repo_full_name": "CAFECA-IO/secret-app" })),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{p}");
    assert_eq!(p["repo_full_name"], "CAFECA-IO/secret-app");
    assert_eq!(p["name"], "secret-app");
    let pid = p["id"].as_str().unwrap().to_string();
    wait_project(&env, &member, &pid).await;
    let log = std::fs::read_to_string(env.dir.join("git.log")).unwrap();
    let fetch = log.lines().find(|l| l.contains(" fetch ") && l.contains("secret-app")).unwrap();
    assert!(
        fetch.ends_with("[auth=Authorization: Basic eC1hY2Nlc3MtdG9rZW46aW5zdC10b2stNDI=]"),
        "{fetch}"
    );
    assert!(
        !log.lines().any(|l| l.split(" [auth=").next().unwrap().contains("inst-tok")),
        "權杖不在參數中"
    );
    // 別人的安裝不能匯入
    let (st, _) = env
        .call(
            "POST",
            "/api/projects",
            &admin,
            Some(json!({ "installation_id": 999, "repo_full_name": "a/b" })),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // webhook：簽章錯誤 → 401；push 到預設分支 → 重新解析；重送不重複
    let push = json!({ "ref": "refs/heads/main", "installation": { "id": 42 },
        "repository": { "full_name": "CAFECA-IO/secret-app", "default_branch": "main" } });
    assert_eq!(
        env.webhook("push", "d-1", &push, "wrong-secret-0000").await,
        StatusCode::UNAUTHORIZED
    );
    let before = log.lines().filter(|l| l.contains(" fetch ")).count();
    assert_eq!(env.webhook("push", "d-1", &push, wh).await, StatusCode::OK);
    wait_project(&env, &member, &pid).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    wait_project(&env, &member, &pid).await;
    assert_eq!(env.webhook("push", "d-1", &push, wh).await, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let log = std::fs::read_to_string(env.dir.join("git.log")).unwrap();
    assert_eq!(log.lines().filter(|l| l.contains(" fetch ")).count(), before + 1, "重送不重複解析");
    // 其他分支的 push 不觸發
    let mut other = push.clone();
    other["ref"] = json!("refs/heads/feature");
    assert_eq!(env.webhook("push", "d-2", &other, wh).await, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let log = std::fs::read_to_string(env.dir.join("git.log")).unwrap();
    assert_eq!(log.lines().filter(|l| l.contains(" fetch ")).count(), before + 1);
    // 移除安裝
    let del = json!({ "action": "deleted", "installation": { "id": 42 } });
    assert_eq!(env.webhook("installation", "d-3", &del, wh).await, StatusCode::OK);
    let (_, list) = env.call("GET", "/api/github/installations", &member, None).await;
    assert!(list.as_array().unwrap().is_empty());
}

// M2-07（NFR-SEC-01）：Origin、速率限制、安全標頭
#[tokio::test]
async fn public_console_guard() {
    let env = Env::new().await;
    let (admin, _) = env.login(acct(0xa1)).await;
    // 跨站 Origin 的寫入 → 403；同源 → 正常
    let post = |origin: &str| {
        env.http
            .post(env.url("/api/tokens"))
            .header("cookie", &admin)
            .header("origin", origin)
            .json(&json!({ "name": "x" }))
            .send()
    };
    assert_eq!(post("https://evil.example").await.unwrap().status(), StatusCode::FORBIDDEN);
    assert_eq!(post(&env.origin).await.unwrap().status(), StatusCode::CREATED);
    // 速率限制：經 Edge（X-Forwarded-For）的登入 nonce 每分鐘 20 次
    let mut last = StatusCode::OK;
    for _ in 0..21 {
        last = env
            .http
            .post(env.url("/api/auth/nonce"))
            .header("x-forwarded-for", "203.0.113.50")
            .send()
            .await
            .unwrap()
            .status();
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    // 其他 IP 不受影響；本機直接連線不限速
    let ok = env
        .http
        .post(env.url("/api/auth/nonce"))
        .header("x-forwarded-for", "203.0.113.51")
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    // 安全標頭
    let res = env.http.get(env.url("/")).send().await.unwrap();
    let h = res.headers();
    assert_eq!(h["x-frame-options"], "DENY");
    assert_eq!(h["x-content-type-options"], "nosniff");
    let csp = h["content-security-policy"].to_str().unwrap();
    assert!(
        csp.contains("default-src 'self'") && csp.contains("frame-ancestors 'none'"),
        "{csp}"
    );
    let res = env.http.get(env.url("/api/me")).header("cookie", &admin).send().await.unwrap();
    assert!(res.headers().get("content-security-policy").is_none());
    assert_eq!(res.headers()["x-content-type-options"], "nosniff");
}
