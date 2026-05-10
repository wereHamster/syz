use axum::http::header;
use axum::http::StatusCode;
use axum::response::IntoResponse;

pub async fn ascii_art() -> impl IntoResponse {
    let art = r#"
This is a syz server.

"#;

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        art,
    )
}
