//! MCP 伺服器（FR-MCP-01～07，ADR 0008，規格見 docs/06-api-spec.md §5）。
//!
//! 只處理協定：JSON-RPC 解析、工具清單、參數檢查、結果與錯誤格式。
//! 所有資源操作都經由 `service`，權限與稽核和 REST 完全相同。
use crate::error::ApiError;
use crate::{service, tokens, AppState, CurrentUser};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use caloud_core::model::Role;
use serde_json::{json, Map, Value};

/// 支援的協定版本，第一個為最新
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];

const INSTRUCTIONS: &str = "Caloud 是自架容器托管平台：上傳 docker compose 檔建立 App 並立即部署，再把網域轉發到 App 的 service:port。\n\
常見流程：\n\
1. 部署：create_app（compose 不可使用 ports、privileged、主機路徑掛載、build）→ list_apps／get_app 查看狀態（deploying → running／failed）→ get_app_logs 排查。\n\
2. 網域：create_domain → 請使用者依回傳的 dns_records 設定 DNS → verify_domain → bind_domain 指定 app_id、service、port。\n\
刪除類工具需要 confirm: true，執行前請先向使用者確認。";

// ───────────────────────── HTTP 進入點 ─────────────────────────

/// `POST /mcp`
pub async fn post(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    // FR-MCP-03：防 DNS rebinding／跨站呼叫
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if origin != s.cfg.public_origin {
            return (StatusCode::FORBIDDEN, Json(json!({ "error": "不允許的 Origin。" }))).into_response();
        }
    }
    let auth = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok());
    let me =
        match tokens::authenticate(&s, auth).await {
            Ok(me) => me,
            Err(ApiError::Unauthorized) => return (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer realm=\"caloud\"")],
                Json(json!({ "error": "請在 Authorization 標頭帶入有效的 API 權杖（Bearer cld_…）。可在 Caloud 的「API 權杖」頁建立。" })),
            )
                .into_response(),
            Err(e) => return e.into_response(),
        };

    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(rpc_error(Value::Null, -32700, "JSON 解析失敗"))).into_response(),
    };
    let Some(obj) = msg.as_object().filter(|o| o.get("jsonrpc") == Some(&json!("2.0"))) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(rpc_error(Value::Null, -32600, "不是合法的 JSON-RPC 2.0 請求（不支援批次）")),
        )
            .into_response();
    };
    let method = obj.get("method").and_then(Value::as_str).unwrap_or_default();
    // notification 或 response：沒有 id，接受後不回應本文
    let Some(id) = obj.get("id").cloned().filter(|_| !method.is_empty()) else {
        return StatusCode::ACCEPTED.into_response();
    };
    let params = obj.get("params").cloned().unwrap_or(json!({}));
    let out = match dispatch(&s, &me, method, &params).await {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(RpcError(code, message)) => rpc_error(id, code, &message),
    };
    Json(out).into_response()
}

/// `GET`／`DELETE /mcp`：不提供 SSE 串流與 session（ADR 0008）
pub async fn method_not_allowed() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")]).into_response()
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// JSON-RPC 協定錯誤
struct RpcError(i64, String);

fn invalid_params(msg: impl Into<String>) -> RpcError {
    RpcError(-32602, msg.into())
}

async fn dispatch(s: &AppState, me: &CurrentUser, method: &str, params: &Value) -> Result<Value, RpcError> {
    match method {
        "initialize" => {
            let requested = params.get("protocolVersion").and_then(Value::as_str).unwrap_or_default();
            let version = PROTOCOL_VERSIONS.iter().find(|v| **v == requested).unwrap_or(&PROTOCOL_VERSIONS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "caloud", "title": "Caloud", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => {
            let list: Vec<Value> = tools()
                .into_iter()
                .filter(|t| me.role().is_admin() || !is_admin_tool(t))
                .map(|t| t.def)
                .collect();
            Ok(json!({ "tools": list }))
        }
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_params("缺少工具名稱 name"))?;
            let tool = tools()
                .into_iter()
                .find(|t| t.def["name"] == name)
                .ok_or_else(|| invalid_params(format!("未知的工具：{name}")))?;
            if is_admin_tool(&tool) && !me.role().is_admin() {
                return Err(invalid_params(format!("未知的工具：{name}")));
            }
            let empty = Map::new();
            let args = match params.get("arguments") {
                None | Some(Value::Null) => &empty,
                Some(Value::Object(m)) => m,
                Some(_) => return Err(invalid_params("arguments 必須是物件")),
            };
            check_arguments(&tool.def, args)?;
            Ok(match call(s, me, name, Args(args)).await? {
                Ok(v) => json!({
                    "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }],
                    "structuredContent": v,
                    "isError": false,
                }),
                // FR-MCP-06：業務錯誤以 isError 回傳，訊息同 REST
                Err(msg) => json!({ "content": [{ "type": "text", "text": msg }], "isError": true }),
            })
        }
        _ => Err(RpcError(-32601, format!("不支援的方法：{method}"))),
    }
}

// ───────────────────────── 工具定義 ─────────────────────────

struct Tool {
    def: Value,
}

fn is_admin_tool(t: &Tool) -> bool {
    matches!(t.def["name"].as_str(), Some("list_users" | "set_user_role"))
}

#[derive(Clone, Copy, Default)]
struct Hints {
    read_only: bool,
    destructive: bool,
    idempotent: bool,
}
const R: Hints = Hints {
    read_only: true,
    destructive: false,
    idempotent: true,
};
const W: Hints = Hints {
    read_only: false,
    destructive: false,
    idempotent: false,
};
const I: Hints = Hints {
    read_only: false,
    destructive: false,
    idempotent: true,
};
const D: Hints = Hints {
    read_only: false,
    destructive: true,
    idempotent: true,
};

/// 定義一個工具；`props` 為 JSON Schema properties，`required` 為必填欄位
fn tool(name: &str, title: &str, description: &str, props: Value, required: &[&str], h: Hints) -> Tool {
    Tool {
        def: json!({
            "name": name,
            "title": title,
            "description": description,
            "inputSchema": { "type": "object", "properties": props, "required": required, "additionalProperties": false },
            "annotations": {
                "title": title,
                "readOnlyHint": h.read_only,
                "destructiveHint": h.destructive,
                "idempotentHint": h.idempotent,
                "openWorldHint": false,
            },
        }),
    }
}

fn tools() -> Vec<Tool> {
    let app_id = json!({ "type": "string", "description": "App ID（UUID），可由 list_apps 取得" });
    let hostname = json!({ "type": "string", "description": "網域主機名稱，例如 blog.example.com" });
    let confirm = json!({ "type": "boolean", "const": true, "description": "必須為 true，表示已向使用者確認要刪除" });
    vec![
        tool("whoami", "目前身分", "回傳此 API 權杖所屬的 Caloud 使用者與角色。", json!({}), &[], R),
        tool(
            "list_apps",
            "列出 App",
            "列出你可管理的 App 與狀態（running／deploying／failed／stopped）。Admin 會看到所有人的 App。",
            json!({ "status": { "type": "string", "enum": ["running", "deploying", "failed", "stopped"], "description": "只列出此狀態" } }),
            &[],
            R,
        ),
        tool(
            "get_app",
            "App 詳細",
            "取得 App 的 compose 原文、service 清單與每個容器的狀態。",
            json!({ "app_id": app_id }),
            &["app_id"],
            R,
        ),
        tool(
            "create_app",
            "建立 App",
            "上傳 docker compose YAML 建立 App，預設立即在背景部署。compose 只能使用 image 與具名 volume；不可使用 ports、privileged、cap_add、主機路徑掛載、build、env_file、container_name。對外流量請用 bind_domain。",
            json!({
                "compose_yaml": { "type": "string", "description": "docker compose YAML 全文" },
                "name": { "type": "string", "description": "App 名稱；省略時以第一個 service 命名" },
                "deploy": { "type": "boolean", "description": "建立後立即部署，預設 true" },
            }),
            &["compose_yaml"],
            W,
        ),
        tool(
            "update_app",
            "更新 App",
            "以新的 compose YAML 取代 App 設定，預設立即重新部署。",
            json!({
                "app_id": app_id,
                "compose_yaml": { "type": "string", "description": "新的 docker compose YAML 全文" },
                "name": { "type": "string", "description": "新的 App 名稱（選填）" },
                "deploy": { "type": "boolean", "description": "更新後立即重新部署，預設 true" },
            }),
            &["app_id", "compose_yaml"],
            W,
        ),
        tool(
            "deploy_app",
            "部署 App",
            "在背景重新部署 App（docker compose up）。之後用 get_app 或 list_apps 查看狀態。",
            json!({ "app_id": app_id }),
            &["app_id"],
            I,
        ),
        tool("stop_app", "停止 App", "停止 App 的所有容器，綁定的網域會停止轉發。", json!({ "app_id": app_id }), &["app_id"], I),
        tool(
            "delete_app",
            "刪除 App",
            "移除 App 與其容器，綁定的網域會停止轉發。預設保留資料 volume；purge_volumes 為 true 時一併刪除資料且無法復原。執行前務必向使用者確認。",
            json!({
                "app_id": app_id,
                "confirm": confirm,
                "purge_volumes": { "type": "boolean", "description": "同時刪除資料 volume（無法復原），預設 false" },
            }),
            &["app_id", "confirm"],
            D,
        ),
        tool(
            "get_app_logs",
            "App log",
            "取得 App 容器最近的 log（含時間戳）。",
            json!({
                "app_id": app_id,
                "service": { "type": "string", "description": "只看此 service；省略時為全部" },
                "tail": { "type": "integer", "minimum": 1, "maximum": 5000, "description": "行數，預設 200，上限 5000" },
            }),
            &["app_id"],
            R,
        ),
        tool("list_domains", "列出網域", "列出托管的網域、驗證狀態、轉發目標與 TLS 狀態。", json!({}), &[], R),
        tool(
            "create_domain",
            "新增網域",
            "新增托管網域，回傳使用者需要到 DNS 服務商設定的紀錄（A 指向 Caloud 主機、TXT 驗證值）。設定完成後呼叫 verify_domain。",
            json!({ "hostname": hostname }),
            &["hostname"],
            W,
        ),
        tool(
            "verify_domain",
            "驗證網域",
            "查詢 TXT 紀錄 _caloud-challenge.<hostname> 驗證擁有權。DNS 生效可能需要數分鐘。",
            json!({ "hostname": hostname }),
            &["hostname"],
            I,
        ),
        tool(
            "bind_domain",
            "綁定網域",
            "把已驗證的網域轉發到 App 的 service 與容器內 port；app_id 為 null 時解除綁定。App 運作中且網域已驗證時立即生效。",
            json!({
                "hostname": hostname,
                "app_id": { "type": ["string", "null"], "description": "目標 App ID；null 表示解除綁定" },
                "service": { "type": "string", "description": "compose 中的 service 名稱" },
                "port": { "type": "integer", "minimum": 1, "maximum": 65535, "description": "容器內監聽的 port" },
            }),
            &["hostname"],
            I,
        ),
        tool(
            "delete_domain",
            "刪除網域",
            "刪除托管網域，該網域會立即停止轉發。執行前務必向使用者確認。",
            json!({ "hostname": hostname, "confirm": confirm }),
            &["hostname", "confirm"],
            D,
        ),
        tool("list_users", "列出使用者", "（Admin）列出所有使用者與角色。", json!({}), &[], R),
        tool(
            "set_user_role",
            "變更角色",
            "（Admin）變更使用者角色。member 可部署；pending 為待核准；disabled 會立即登出並使其 API 權杖失效。只有 Super Admin 能授予或撤銷 admin。",
            json!({
                "account": { "type": "string", "description": "使用者的 CAFECA 帳戶地址（0x…）" },
                "role": { "type": "string", "enum": ["admin", "member", "pending", "disabled"] },
            }),
            &["account", "role"],
            I,
        ),
    ]
}

/// 依 inputSchema 檢查必填與未知欄位（FR-MCP-06：參數錯誤為協定錯誤 -32602）
fn check_arguments(def: &Value, args: &Map<String, Value>) -> Result<(), RpcError> {
    let schema = &def["inputSchema"];
    let props = schema["properties"].as_object().cloned().unwrap_or_default();
    if let Some(k) = args.keys().find(|k| !props.contains_key(*k)) {
        return Err(invalid_params(format!("未知的參數：{k}")));
    }
    for r in schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        if args.get(r).is_none_or(Value::is_null) {
            return Err(invalid_params(format!("缺少必填參數：{r}")));
        }
    }
    Ok(())
}

struct Args<'a>(&'a Map<String, Value>);

impl Args<'_> {
    fn str(&self, k: &str) -> Result<&str, RpcError> {
        self.opt_str(k)?.ok_or_else(|| invalid_params(format!("缺少必填參數：{k}")))
    }
    fn opt_str(&self, k: &str) -> Result<Option<&str>, RpcError> {
        match self.0.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(invalid_params(format!("{k} 必須是字串"))),
        }
    }
    fn opt_bool(&self, k: &str) -> Result<Option<bool>, RpcError> {
        match self.0.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(invalid_params(format!("{k} 必須是布林值"))),
        }
    }
    fn opt_u64(&self, k: &str, max: u64) -> Result<Option<u64>, RpcError> {
        match self.0.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_u64()
                .filter(|n| (1..=max).contains(n))
                .map(Some)
                .ok_or_else(|| invalid_params(format!("{k} 必須是 1 到 {max} 的整數"))),
        }
    }
}

/// 外層 Err：協定錯誤；內層 Err：工具執行失敗（isError）
type CallResult = Result<Result<Value, String>, RpcError>;

fn biz<T: serde::Serialize>(r: Result<T, ApiError>) -> Result<Value, String> {
    r.map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
        .map_err(|e| e.public_message())
}

async fn call(s: &AppState, me: &CurrentUser, name: &str, a: Args<'_>) -> CallResult {
    Ok(match name {
        "whoami" => biz(me.require_deploy().map(|_| me.0.clone())),
        "list_apps" => {
            let status = a.opt_str("status")?;
            biz(service::list_apps(s, me).await.map(|apps| {
                let apps: Vec<_> = apps.into_iter().filter(|x| status.is_none_or(|st| x.status == st)).collect();
                json!({ "apps": apps })
            }))
        }
        "get_app" => biz(service::get_app(s, me, a.str("app_id")?).await),
        "create_app" => {
            let name = a.opt_str("name")?.map(str::to_string);
            biz(service::create_app(s, me, name, a.str("compose_yaml")?, a.opt_bool("deploy")?.unwrap_or(true)).await)
        }
        "update_app" => {
            let name = a.opt_str("name")?.map(str::to_string);
            let deploy = a.opt_bool("deploy")?.unwrap_or(true);
            biz(service::update_app(s, me, a.str("app_id")?, name, a.str("compose_yaml")?, deploy).await)
        }
        "deploy_app" => {
            let id = a.str("app_id")?;
            biz(service::deploy_app(s, me, id)
                .await
                .map(|_| json!({ "app_id": id, "status": "deploying" })))
        }
        "stop_app" => {
            let id = a.str("app_id")?;
            biz(service::stop_app(s, me, id)
                .await
                .map(|_| json!({ "app_id": id, "status": "stopped" })))
        }
        "delete_app" => {
            let id = a.str("app_id")?;
            if a.opt_bool("confirm")? != Some(true) {
                return Ok(Err(format!("刪除 App {id} 需要確認：請向使用者確認後，以 confirm: true 再呼叫。")));
            }
            let purge = a.opt_bool("purge_volumes")?.unwrap_or(false);
            biz(service::delete_app(s, me, id, purge).await.map(|_| json!({ "deleted": id })))
        }
        "get_app_logs" => {
            let tail = a.opt_u64("tail", 5000)?.map(|n| n as u32);
            biz(service::app_logs(s, me, a.str("app_id")?, a.opt_str("service")?, tail)
                .await
                .map(|logs| json!({ "logs": logs })))
        }
        "list_domains" => biz(service::list_domains(s, me).await.map(|d| json!({ "domains": d }))),
        "create_domain" => biz(service::create_domain(s, me, a.str("hostname")?).await.map(|d| {
            let records = service::dns_records(&d);
            let mut v = serde_json::to_value(&d).unwrap_or(Value::Null);
            v["dns_records"] = records;
            v
        })),
        "verify_domain" => biz(service::verify_domain(s, me, a.str("hostname")?).await),
        "bind_domain" => {
            let port = a.opt_u64("port", 65535)?.map(|n| n as u16);
            biz(service::bind_domain(s, me, a.str("hostname")?, a.opt_str("app_id")?, a.opt_str("service")?, port).await)
        }
        "delete_domain" => {
            let host = a.str("hostname")?;
            if a.opt_bool("confirm")? != Some(true) {
                return Ok(Err(format!(
                    "刪除網域 {host} 需要確認：請向使用者確認後，以 confirm: true 再呼叫。"
                )));
            }
            biz(service::delete_domain(s, me, host).await.map(|_| json!({ "deleted": host })))
        }
        "list_users" => biz(service::list_users(s, me).await.map(|u| json!({ "users": u }))),
        "set_user_role" => {
            let role = Role::parse(a.str("role")?)
                .filter(|r| *r != Role::SuperAdmin)
                .ok_or_else(|| invalid_params("role 必須是 admin、member、pending 或 disabled"))?;
            biz(service::set_user_role(s, me, a.str("account")?, role).await)
        }
        _ => return Err(invalid_params(format!("未知的工具：{name}"))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // FR-MCP-04、FR-MCP-05：每個工具都有完整中繼資料，刪除類工具標示 destructive 且要求 confirm
    #[test]
    fn tool_definitions_are_complete() {
        let list = tools();
        let mut names: Vec<&str> = list.iter().map(|t| t.def["name"].as_str().unwrap()).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n, "工具名稱不可重複");
        for t in &list {
            let d = &t.def;
            assert!(!d["description"].as_str().unwrap().is_empty());
            assert_eq!(d["inputSchema"]["additionalProperties"], false);
            let destructive = d["annotations"]["destructiveHint"].as_bool().unwrap();
            let needs_confirm = d["inputSchema"]["required"].as_array().unwrap().contains(&json!("confirm"));
            assert_eq!(destructive, needs_confirm, "{}", d["name"]);
        }
    }
}
