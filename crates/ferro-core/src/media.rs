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
    Some(render_markdown_text(&text))
}

/// Render + sanitize arbitrary markdown (comment previews, AI answers).
pub fn render_markdown_text(text: &str) -> String {
    let opts = pulldown_cmark::Options::all();
    let parser = pulldown_cmark::Parser::new_ext(text, opts);
    let mut html = String::with_capacity(text.len());
    pulldown_cmark::html::push_html(&mut html, parser);
    sanitize_html(&html)
}

/// Allowlist sanitizer per API.md § 5.4 (B0: no rewrites, those land in B1).
/// Everything else — scripts, event handlers, iframes, inline SVG,
/// `javascript:` URLs, forms — is removed.
pub fn sanitize_html(html: &str) -> String {
    use std::borrow::Cow;
    use std::collections::HashSet;
    let tags: HashSet<&str> = [
        "p",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "a",
        "img",
        "ul",
        "ol",
        "li",
        "input",
        "blockquote",
        "pre",
        "code",
        "table",
        "thead",
        "tbody",
        "tr",
        "th",
        "td",
        "em",
        "strong",
        "del",
        "sup",
        "sub",
        "br",
        "hr",
        "details",
        "summary",
        "kbd",
        "span",
        "div",
    ]
    .into_iter()
    .collect();
    let schemes: HashSet<&str> = ["http", "https", "mailto"].into_iter().collect();
    ammonia::Builder::default()
        .tags(tags)
        .url_schemes(schemes)
        .add_tag_attributes("a", &["href", "title"])
        .add_tag_attributes("img", &["src", "alt", "title", "width", "height"])
        .add_tag_attributes("h1", &["id"])
        .add_tag_attributes("h2", &["id"])
        .add_tag_attributes("h3", &["id"])
        .add_tag_attributes("h4", &["id"])
        .add_tag_attributes("h5", &["id"])
        .add_tag_attributes("h6", &["id"])
        .add_tag_attributes("input", &["type", "checked", "disabled"])
        .add_tag_attributes("th", &["align"])
        .add_tag_attributes("td", &["align"])
        .add_tag_attributes("code", &["class"])
        .add_tag_attributes("div", &["class"])
        .attribute_filter(|element, attribute, value| {
            let ok = match (element, attribute) {
                // `id` only on headings (B1 adds GitHub slugs; B0 keeps author ids).
                (e, "id") if e.starts_with('h') && e.len() == 2 => true,
                (_, "id") => false,
                ("a", "href") => {
                    value.starts_with("http://")
                        || value.starts_with("https://")
                        || value.starts_with("mailto:")
                }
                ("img", "src") => {
                    // Relative paths and https only. No data:/javascript:/vbscript:.
                    !(value.contains(':')
                        && !value.starts_with("https://")
                        && !value.starts_with("http://"))
                }
                ("input", "type") => value == "checkbox",
                ("code", "class") => value.split_whitespace().all(|c| {
                    c.strip_prefix("language-")
                        .map(|l| {
                            !l.is_empty()
                                && l.chars()
                                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                        })
                        .unwrap_or(false)
                }),
                ("div", "class") => {
                    value == "md-suggestion"
                        || value == "md-alert"
                        || value.starts_with("md-alert md-alert-")
                            && ["note", "tip", "important", "warning", "caution"].contains(
                                &value
                                    .rsplit(' ')
                                    .next()
                                    .unwrap_or("")
                                    .strip_prefix("md-alert-")
                                    .unwrap_or(""),
                            )
                }
                _ => true,
            };
            ok.then_some(Cow::Borrowed(value))
        })
        .clean(html)
        .to_string()
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

    #[test]
    fn xss_corpus_stripped() {
        let cases = [
            // script + event handlers
            "<script>alert(1)</script><p>ok</p>",
            "<img src=x onerror=alert(1)>",
            "<svg onload=alert(1)><p>ok</p>",
            "<a href=\"javascript:alert(1)\">x</a>",
            "<iframe src=\"https://x\"></iframe>",
            "<p style=\"color:red\" onclick=\"a()\">x</p>",
            "<form action=\"/x\"><input type=\"text\"></form>",
            "<a href=\"https://ok.example\">keep</a>",
            "<input type=\"checkbox\" checked disabled>",
            "<input type=\"text\" value=\"y\">",
            "<code class=\"language-rust\">x</code>",
            "<code class=\"evil\" onmouseover=\"a()\">x</code>",
            "<div class=\"md-alert md-alert-note\">n</div>",
            "<div class=\"other\">o</div>",
            "<h1 id=\"a\">t</h1><p id=\"b\">u</p>",
            "<img src=\"https://x/y.png\" alt=\"i\">",
            "<img src=\"data:text/html,<script>alert(1)</script>\">",
        ];
        for html in cases {
            let out = sanitize_html(html);
            assert!(!out.contains("alert("), "leak in {html:?} -> {out:?}");
            assert!(!out.contains("<script"), "{html:?} -> {out:?}");
            assert!(
                !out.contains("onerror")
                    && !out.contains("onload")
                    && !out.contains("onclick")
                    && !out.contains("onmouseover"),
                "{html:?} -> {out:?}"
            );
            assert!(!out.contains("javascript:"), "{html:?} -> {out:?}");
            assert!(
                !out.contains("<iframe") && !out.contains("<form") && !out.contains("<svg"),
                "{html:?} -> {out:?}"
            );
            assert!(!out.contains("style="), "{html:?} -> {out:?}");
        }
        // Allowed content survives.
        assert!(sanitize_html("<a href=\"https://ok.example\">keep</a>")
            .contains("href=\"https://ok.example\""));
        assert!(sanitize_html("<input type=\"checkbox\" checked disabled>").contains("checked"));
        assert!(!sanitize_html("<input type=\"text\" value=\"y\">").contains("type=\"text\""));
        assert!(sanitize_html("<code class=\"language-rust\">x</code>").contains("language-rust"));
        assert!(!sanitize_html("<code class=\"evil\">x</code>").contains("class="));
        assert!(
            sanitize_html("<div class=\"md-alert md-alert-note\">n</div>")
                .contains("md-alert-note")
        );
        assert!(!sanitize_html("<div class=\"other\">o</div>").contains("class="));
        assert!(sanitize_html("<h1 id=\"a\">t</h1>").contains("id=\"a\""));
        assert!(!sanitize_html("<p id=\"b\">u</p>").contains("id="));
        assert!(sanitize_html("<img src=\"https://x/y.png\" alt=\"i\">").contains("src="));
        assert!(!sanitize_html("<img src=\"data:text/html,x\">").contains("src="));
    }

    #[test]
    fn markdown_pipeline_is_sanitized() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("x.md"),
            "# T\n\n<img src=x onerror=alert(1)>\n\n[evil](javascript:alert(2))\n",
        )
        .unwrap();
        let idx = Index::new(dir.path().to_path_buf());
        let html = render_markdown(&idx, "x.md").unwrap();
        assert!(html.contains("<h1>"));
        assert!(!html.contains("alert("));
        assert!(!html.contains("javascript:"));
    }
}
