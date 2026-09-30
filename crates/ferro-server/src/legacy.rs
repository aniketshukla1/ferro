//! Embedded `web/` static assets and `--dev-web` disk serving (B8: legacy `/api/*` removed).

use axum::{
    extract::Extension,
    http::{header, StatusCode},
    response::IntoResponse,
};
/// The product UI, gzipped at build time (build.rs): `(path, gzip bytes, weak ETag)`, sorted by
/// path. The e2e suite and the `?mock=1` backend never ship (BACKEND.md B1); `--dev-web` still
/// serves them from disk.
mod web {
    include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));
}

fn embedded(path: &str) -> Option<&'static (&'static str, &'static [u8], &'static str)> {
    web::WEB
        .binary_search_by(|(p, _, _)| (*p).cmp(path))
        .ok()
        .map(|i| &web::WEB[i])
}

#[derive(Debug, Clone)]
pub struct DevWeb(pub Option<String>);

pub(crate) async fn static_file(
    Extension(g): Extension<std::sync::Arc<crate::guard::GuardConfig>>,
    Extension(dev): Extension<DevWeb>,
    req: axum::http::Request<axum::body::Body>,
) -> impl IntoResponse {
    if let Some(res) = crate::guard::bootstrap(&g, &req) {
        return res;
    }
    let mut path = req.uri().path().trim_start_matches('/').to_string();
    if path.is_empty() {
        path = "index.html".to_string();
    }
    if path.contains("..") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    // --dev-web: serve from disk with no-store so the frontend iterates
    // without rebuilding. Otherwise serve embedded assets with ETags.
    if let Some(root) = dev.0.as_deref() {
        let full = std::path::PathBuf::from(root).join(&path);
        match std::fs::read(&full) {
            Ok(bytes) => {
                let mime = mime_guess(&path);
                return (
                    [
                        (header::CONTENT_TYPE, mime),
                        (header::CACHE_CONTROL, "no-store"),
                    ],
                    bytes,
                )
                    .into_response();
            }
            Err(_) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        }
    }
    let Some((_, gz, etag)) = embedded(&path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    if req
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == *etag)
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let gzip_ok = req
        .headers()
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|e| e.trim().starts_with("gzip")));
    let headers = [
        (header::CONTENT_TYPE, mime_guess(&path)),
        (header::ETAG, *etag),
        (header::CACHE_CONTROL, "no-cache"),
        (header::VARY, "Accept-Encoding"),
    ];
    if gzip_ok {
        return (headers, [(header::CONTENT_ENCODING, "gzip")], *gz).into_response();
    }
    // Rare (curl, old proxies): inflate on the fly.
    let mut raw = Vec::new();
    match std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(*gz), &mut raw) {
        Ok(_) => (headers, raw).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "corrupt embedded asset").into_response(),
    }
}

fn mime_guess(p: &str) -> &'static str {
    if p.ends_with(".html") {
        "text/html"
    } else if p.ends_with(".js") {
        "text/javascript"
    } else if p.ends_with(".css") {
        "text/css"
    } else if p.ends_with(".json") {
        "application/json"
    } else if p.ends_with(".svg") {
        "image/svg+xml"
    } else {
        "application/octet-stream"
    }
}

#[cfg(test)]
mod tests {
    use super::web::WEB;

    /// BACKEND.md B1: the binary ships the product UI only — not the e2e suite (and its
    /// node_modules) or the mock backend, which must never load in a real session.
    #[test]
    fn embedded_web_excludes_tests_and_mock() {
        let shipped: Vec<String> = WEB.iter().map(|(p, _, _)| p.to_string()).collect();
        assert!(
            shipped.windows(2).all(|w| w[0] < w[1]),
            "the table is sorted for binary search"
        );
        let leaked: Vec<&String> = shipped
            .iter()
            .filter(|p| p.starts_with("tests/") || p.starts_with("src/mock/"))
            .collect();
        assert!(
            leaked.is_empty(),
            "embedded dev-only files: {:?}",
            &leaked[..leaked.len().min(5)]
        );
        for needed in ["index.html", "src/main.js", "styles/tokens.css"] {
            assert!(
                shipped.iter().any(|p| p == needed),
                "{needed} missing from the embed"
            );
        }
    }
}
