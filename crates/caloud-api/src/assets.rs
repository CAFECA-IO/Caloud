//! Design System 靜態檔（ADR 0006）：編譯時內嵌，經 `/assets/{*path}` 提供。
use axum::extract::Path;
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};

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
];

pub async fn serve(Path(path): Path<String>) -> Response {
    match FILES.iter().find(|(p, _, _)| *p == path) {
        Some((_, ct, body)) => ([(header::CONTENT_TYPE, *ct), (header::CACHE_CONTROL, "no-cache")], *body).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn index() -> Html<&'static str> {
    Html(include_str!("../../../web/index.html"))
}

/// 元件總覽頁（Design System 參考，FR-UI-12）
pub async fn styleguide() -> Html<&'static str> {
    Html(include_str!("../../../web/styleguide.html"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
