//! Design System 靜態檔（ADR 0006）：編譯時內嵌，經 `/assets/{*path}` 提供。
use axum::extract::{Path, RawQuery};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use caloud_core::build;
use std::sync::LazyLock;

/// 允許提供的檔案清單；新增檔案需同步更新 docs/06-api-spec.md §4
const FILES: &[(&str, &str, &str)] = &[
    (
        "ds/tokens.css",
        "text/css; charset=utf-8",
        include_str!("../../../web/ds/tokens.css"),
    ),
    (
        "ds/components.css",
        "text/css; charset=utf-8",
        include_str!("../../../web/ds/components.css"),
    ),
    ("ds/theme.css", "text/css; charset=utf-8", include_str!("../../../web/ds/theme.css")),
    ("ds/ui.js", "text/javascript; charset=utf-8", include_str!("../../../web/ds/ui.js")),
    // 品牌標誌（docs/04 §4.1）
    ("ds/logo.svg", "image/svg+xml", include_str!("../../../web/ds/logo.svg")),
];

/// ADR 0023：網址帶目前的 `v=<commit>` 時長期快取；其他（含 `dev`）每次重新確認
pub async fn serve(Path(path): Path<String>, RawQuery(q): RawQuery) -> Response {
    let versioned = build::released() && q.as_deref() == Some(&format!("v={}", build::COMMIT));
    let cache = if versioned {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    match FILES.iter().find(|(p, _, _)| *p == path) {
        Some((_, ct, body)) => ([(header::CONTENT_TYPE, *ct), (header::CACHE_CONTROL, cache)], *body).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// 管理介面：資源網址加上 `?v=<commit>`，並以 meta 標示載入的版本（ADR 0023）
static INDEX: LazyLock<String> = LazyLock::new(|| {
    let mut html = include_str!("../../../web/index.html").replace(
        "<head>",
        &format!("<head>\n<meta name=\"caloud-build\" content=\"{}\">", build::COMMIT),
    );
    for (p, _, _) in FILES {
        html = html.replace(&format!("/assets/{p}\""), &format!("/assets/{p}?v={}\"", build::COMMIT));
    }
    html
});

/// `index.html` 每次重新確認；ETag 為 commit，相符時回 304
pub async fn index(headers: HeaderMap) -> Response {
    let etag = format!("\"{}\"", build::COMMIT);
    let fresh = build::released()
        && headers
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(',').any(|t| t.trim() == etag));
    let h = [(header::CACHE_CONTROL, "no-cache".to_string()), (header::ETAG, etag)];
    if fresh {
        return (StatusCode::NOT_MODIFIED, h).into_response();
    }
    (h, Html(INDEX.as_str())).into_response()
}

/// `GET /api/version`（不需登入，FR-OPS-15）
pub async fn version() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "version": build::VERSION, "commit": build::COMMIT, "built_at": build::BUILT_AT }))
}

/// 元件總覽頁（Design System 參考，FR-UI-12）
pub async fn styleguide() -> Html<&'static str> {
    Html(include_str!("../../../web/styleguide.html"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ADR 0023：index 的資源都帶版本，並標示載入的版本
    #[test]
    fn index_references_are_versioned() {
        let v = format!("?v={}", build::COMMIT);
        assert!(INDEX.contains(&format!("<meta name=\"caloud-build\" content=\"{}\">", build::COMMIT)));
        for part in INDEX.split("/assets/").skip(1) {
            let path = part.split('"').next().unwrap();
            assert!(path.ends_with(&v), "unversioned asset {path}");
        }
    }

    // FR-UI-04：每個 Design System 檔案都有內容，且 index.html 引用的資源都在清單中
    #[test]
    fn index_assets_are_embedded() {
        let html = include_str!("../../../web/index.html");
        for part in html.split("/assets/").skip(1) {
            let path = part.split('"').next().unwrap();
            assert!(FILES.iter().any(|(p, _, b)| *p == path && !b.is_empty()), "missing asset {path}");
        }
    }
}
