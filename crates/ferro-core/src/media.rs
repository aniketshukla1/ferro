//! Markdown + image helpers. GFM render via pulldown-cmark;
//! images served as bytes with a content-type guess.

use crate::index::Index;
pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg", "ico", "bmp"];

pub fn is_image(path: &str) -> bool {
    path.rsplit('.')
        .next()
        .map(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

pub fn is_markdown(path: &str) -> bool {
    matches!(
        path.rsplit('.')
            .next()
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("md") | Some("markdown")
    )
}

pub fn content_type(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("bmp") => "image/bmp",
        _ => "application/octet-stream",
    }
}

pub fn render_markdown(idx: &Index, rel: &str) -> Option<String> {
    let p = idx.safe_join(rel)?;
    if p.metadata().map(|m| m.len() > 2_000_000).unwrap_or(true) {
        return None;
    }
    let text = std::fs::read_to_string(&p).ok()?;
    let opts = pulldown_cmark::Options::all();
    let parser = pulldown_cmark::Parser::new_ext(&text, opts);
    let mut html = String::with_capacity(text.len());
    pulldown_cmark::html::push_html(&mut html, parser);
    Some(html)
}

pub fn read_image_bytes(idx: &Index, rel: &str) -> Option<(Vec<u8>, &'static str)> {
    let p = idx.safe_join(rel)?;
    if p.metadata()
        .map(|m| m.len() > 10 * 1024 * 1024)
        .unwrap_or(true)
    {
        return None;
    }
    let bytes = std::fs::read(&p).ok()?;
    Some((bytes, content_type(rel)))
}

/// Base64 data URL for Tauri (no asset-protocol allowlist needed).
pub fn image_data_url(idx: &Index, rel: &str) -> Option<String> {
    // Minimal base64 without a new dep.
    let (bytes, mime) = read_image_bytes(idx, rel)?;
    Some(format!("data:{mime};base64,{}", base64_encode(&bytes)))
}

fn base64_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len() / 3 * 4 + 4);
    for chunk in input.chunks(3) {
        let mut n: u32 = 0;
        for (i, &b) in chunk.iter().enumerate() {
            n |= (b as u32) << (16 - 8 * i);
        }
        let pad = 3 - chunk.len();
        for i in 0..4 - pad {
            out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
        for _ in 0..pad {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_gfm() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "# Hi\n\n- one\n- two\n\n`code`\n").unwrap();
        let idx = Index::new(dir.path().to_path_buf());
        let html = render_markdown(&idx, "a.md").unwrap();
        assert!(html.contains("<h1>"));
        assert!(html.contains("<li>"));
        assert!(html.contains("<code>"));
        assert!(is_markdown("a.md"));
        assert!(is_image("x.png"));
        assert!(!is_image("a.rs"));
    }

    #[test]
    fn base64_roundtrip_shape() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
    }
}
