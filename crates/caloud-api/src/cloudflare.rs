//! Cloudflare DNS API（ADR 0016）：驗證權杖、寫入與刪除 `_acme-challenge` TXT。
//!
//! 錯誤訊息只帶 Cloudflare 回傳的錯誤，不含權杖。
use serde_json::{json, Value};

pub struct Cloudflare<'a> {
    pub http: &'a reqwest::Client,
    pub base: &'a str,
    pub token: &'a str,
}

impl Cloudflare<'_> {
    async fn call(&self, req: reqwest::RequestBuilder) -> anyhow::Result<Value> {
        let res = req
            .bearer_auth(self.token)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("無法連線 Cloudflare API：{}", e.without_url()))?;
        let status = res.status();
        let v: Value = res.json().await.unwrap_or(Value::Null);
        if !status.is_success() || v["success"] != Value::Bool(true) {
            let msg = v["errors"]
                .as_array()
                .map(|errs| {
                    errs.iter()
                        .map(|e| format!("{} {}", e["code"], e["message"].as_str().unwrap_or_default()))
                        .collect::<Vec<_>>()
                        .join("；")
                })
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| format!("HTTP {status}"));
            anyhow::bail!("Cloudflare API 錯誤：{msg}");
        }
        Ok(v)
    }

    /// 以權杖查詢 zone，回傳 zone id（FR-OPS-03：設定權杖時驗證）
    pub async fn zone_id(&self, zone: &str) -> anyhow::Result<String> {
        let v = self
            .call(self.http.get(format!("{}/zones", self.base)).query(&[("name", zone)]))
            .await?;
        v["result"][0]["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("此權杖看不到 zone {zone}，請確認權杖可讀取該 zone 並可編輯 DNS。"))
    }

    /// 新增 TXT 記錄，回傳記錄 id
    pub async fn create_txt(&self, zone_id: &str, name: &str, content: &str) -> anyhow::Result<String> {
        let v = self
            .call(
                self.http
                    .post(format!("{}/zones/{zone_id}/dns_records", self.base))
                    .json(&json!({ "type": "TXT", "name": name, "content": content, "ttl": 60 })),
            )
            .await?;
        v["result"]["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("Cloudflare 沒有回傳記錄 id"))
    }

    pub async fn delete_record(&self, zone_id: &str, id: &str) -> anyhow::Result<()> {
        self.call(self.http.delete(format!("{}/zones/{zone_id}/dns_records/{id}", self.base)))
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path, Query};
    use axum::http::HeaderMap;
    use axum::routing::{delete, get, post};
    use axum::{Json, Router};
    use std::collections::HashMap;

    async fn mock() -> String {
        fn authed(h: &HeaderMap) -> bool {
            h.get("authorization").and_then(|v| v.to_str().ok()) == Some("Bearer good")
        }
        let app = Router::new()
            .route(
                "/zones",
                get(|h: HeaderMap, Query(q): Query<HashMap<String, String>>| async move {
                    if !authed(&h) {
                        return Json(json!({ "success": false, "errors": [{ "code": 1000, "message": "Invalid API Token" }] }));
                    }
                    let result = if q.get("name").map(String::as_str) == Some("caloud.tw") {
                        json!([{ "id": "zone1", "name": "caloud.tw" }])
                    } else {
                        json!([])
                    };
                    Json(json!({ "success": true, "errors": [], "result": result }))
                }),
            )
            .route(
                "/zones/{zone}/dns_records",
                post(|Path(zone): Path<String>, Json(b): Json<Value>| async move {
                    assert_eq!(zone, "zone1");
                    assert_eq!(b["type"], "TXT");
                    Json(json!({ "success": true, "result": { "id": "rec1" } }))
                }),
            )
            .route(
                "/zones/{zone}/dns_records/{id}",
                delete(
                    |Path((_, id)): Path<(String, String)>| async move { Json(json!({ "success": id == "rec1", "result": { "id": id } })) },
                ),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await });
        format!("http://{addr}")
    }

    // FR-OPS-03、FR-OPS-10
    #[tokio::test]
    async fn verifies_token_and_manages_txt() {
        let base = mock().await;
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let cf = Cloudflare {
            http: &http,
            base: &base,
            token: "good",
        };
        assert_eq!(cf.zone_id("caloud.tw").await.unwrap(), "zone1");
        assert!(cf.zone_id("other.tw").await.unwrap_err().to_string().contains("看不到 zone"));
        assert_eq!(cf.create_txt("zone1", "_acme-challenge.caloud.tw", "v").await.unwrap(), "rec1");
        cf.delete_record("zone1", "rec1").await.unwrap();
        assert!(cf.delete_record("zone1", "nope").await.is_err());

        let bad = Cloudflare {
            http: &http,
            base: &base,
            token: "bad",
        };
        let e = bad.zone_id("caloud.tw").await.unwrap_err().to_string();
        assert!(e.contains("Invalid API Token") && !e.contains("bad"), "{e}");
    }
}
