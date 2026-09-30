//! Markdown v2 (API.md §§ 4.5, 5.4): GFM render with heading ids,
//! data-line source mapping, workspace link/image rewriting, external-image
//! opt-in, GitHub alerts, highlighted fences, suggestion previews, ammonia gate.

use axum::{
    body::Bytes as AxumBytes,
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/file/markdown", get(file_markdown))
        .route("/api/v1/markdown/render", post(render))
}

#[derive(Deserialize)]
struct MdQ {
    path: String,
}

/// GitHub slug rules: lowercase, spaces→`-`, drop punctuation, keep Unicode.
pub fn slugify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_alphanumeric() || c == '_' || c == '-' || c == ' ' {
            out.push(if c == ' ' { '-' } else { c });
        }
    }
    out
}

fn offset_line(source: &str, byte: usize) -> usize {
    source[..byte.min(source.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count()
        + 1
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Block {
    Heading(u32),
    Para,
    Pre,
    Quote,
    List(bool),
    Item,
    Table,
    Hr,
}

struct DocBlocks {
    /// (block kind, 1-based source line) in document order.
    blocks: Vec<(Block, usize)>,
    headings: Vec<(u32, String, usize)>,
    fences: Vec<(String, String, usize)>,
}

fn scan_blocks(source: &str) -> DocBlocks {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
    let mut blocks = Vec::new();
    let mut headings = Vec::new();
    let mut fences: Vec<(String, String, usize)> = Vec::new();
    let mut fence_lang = String::new();
    let mut fence_text = String::new();
    let mut in_fence = false;
    for (ev, range) in Parser::new_ext(source, Options::all()).into_offset_iter() {
        let line = offset_line(source, range.start);
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                let n = match level {
                    pulldown_cmark::HeadingLevel::H1 => 1,
                    pulldown_cmark::HeadingLevel::H2 => 2,
                    pulldown_cmark::HeadingLevel::H3 => 3,
                    pulldown_cmark::HeadingLevel::H4 => 4,
                    pulldown_cmark::HeadingLevel::H5 => 5,
                    pulldown_cmark::HeadingLevel::H6 => 6,
                };
                blocks.push((Block::Heading(n), line));
            }
            Event::Start(Tag::Paragraph) => blocks.push((Block::Para, line)),
            Event::Start(Tag::BlockQuote(_)) => blocks.push((Block::Quote, line)),
            Event::Start(Tag::List(n)) => {
                blocks.push((Block::List(n.is_some()), line));
            }
            Event::Start(Tag::Item) => blocks.push((Block::Item, line)),
            Event::Start(Tag::Table(_)) => blocks.push((Block::Table, line)),
            Event::Rule => blocks.push((Block::Hr, line)),
            Event::Start(Tag::CodeBlock(kind)) => {
                in_fence = true;
                fence_lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().unwrap_or("").to_string()
                    }
                    pulldown_cmark::CodeBlockKind::Indented => String::new(),
                };
                fence_text.clear();
                blocks.push((Block::Pre, line));
            }
            Event::Text(t) if in_fence => fence_text.push_str(&t),
            Event::End(TagEnd::CodeBlock) => {
                in_fence = false;
                fences.push((fence_lang.clone(), fence_text.clone(), line));
            }

            _ => {}
        }
    }
    // Headings text: re-scan source lines is overkill; text comes from HTML pairing below.
    // Record (level, line) here; text filled by caller matching <hN> order.
    for (b, line) in &blocks {
        if let Block::Heading(n) = b {
            headings.push((*n, String::new(), *line));
        }
    }
    // Lists: pulldown emits <ol> for ordered lists; fix the tag now.
    DocBlocks {
        blocks,
        headings,
        fences,
    }
}

/// Render markdown to sanitized v2 HTML + headings + external image count.
/// `base_dir`: workspace-relative dir of the source file (for link/image targets).
/// `raw_base`: prefix for rewritten workspace image URLs.
pub fn render_v2(
    source: &str,
    base_dir: &str,
    raw_base: &str,
    ctx: Option<&RenderContextDoc>,
) -> Rendered {
    use pulldown_cmark::{html, Options, Parser};
    let scanned = scan_blocks(source);
    let mut html = String::new();
    html::push_html(&mut html, Parser::new_ext(source, Options::all()));
    let mut html = post_process(html, &scanned, source, base_dir, raw_base, ctx);
    // Headings text from the final HTML headings in order.
    let mut headings = Vec::new();
    let mut search = html.as_str();
    for (level, _, line) in &scanned.headings {
        let open = format!("<h{level}");
        let Some(s) = search.find(&open) else { break };
        let after = &search[s..];
        let Some(gt) = after.find('>') else { break };
        let rest = &after[gt + 1..];
        let end_tag = format!("</h{level}>");
        let Some(e) = rest.find(&end_tag) else { break };
        let text = strip_tags(&rest[..e]);
        headings.push((*level, text.clone(), *line));
        search = &rest[e + end_tag.len()..];
    }
    html = sanitize_v2(&html);
    let external = count_external_images(&html);
    Rendered {
        html,
        headings: headings
            .into_iter()
            .map(|(level, text, line)| (level, text.clone(), slugify(&text), line))
            .collect(),
        external_images: external,
    }
}

pub struct Rendered {
    pub html: String,
    pub headings: Vec<(u32, String, String, usize)>,
    pub external_images: usize,
}

pub struct RenderContextDoc {
    pub path: Option<String>,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    html_unescape(&out)
}

fn html_unescape(s: &str) -> String {
    // `&amp;` last: replacing it first would turn `&amp;lt;` into `<` (double unescape).
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Resolve a workspace link or image path against the document's directory,
/// applying `.` and `..` (a leading `/` is repo-root relative, as on GitHub).
/// `None` when the path climbs above the workspace root.
fn join_workspace(base_dir: &str, rel: &str) -> Option<String> {
    let mut out: Vec<&str> = if rel.starts_with('/') {
        Vec::new()
    } else {
        base_dir.split('/').filter(|s| !s.is_empty()).collect()
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => out.push(s),
        }
    }
    (!out.is_empty()).then(|| out.join("/"))
}

/// Decode `%XX` escapes in a link destination (`my%20file.md` → `my file.md`).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Encode a workspace path as a query value (keeps `/` and unreserved bytes).
fn query_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &c in s.as_bytes() {
        if c.is_ascii_alphanumeric() || b"-._~/".contains(&c) {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

/// Count `<img>` tags that wait for the external-image opt-in. Runs on sanitized
/// HTML, where text and code can no longer contain a literal `<img`.
fn count_external_images(html: &str) -> usize {
    let mut n = 0;
    let mut rest = html;
    while let Some(i) = rest.find("<img") {
        let end = rest[i..].find('>').map_or(rest.len(), |e| i + e);
        if rest[i..end].contains(" data-ext-src=\"") {
            n += 1;
        }
        rest = &rest[end..];
    }
    n
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Structural post-processing before sanitizing: heading ids + data-line,
/// code fence highlighting, alerts, link/image rewrites, suggestions.
fn post_process(
    mut html: String,
    scanned: &DocBlocks,
    source: &str,
    base_dir: &str,
    raw_base: &str,
    ctx: Option<&RenderContextDoc>,
) -> String {
    // 1. Headings: <hN>..</hN> -> <hN id data-line>, paired in order.
    let mut spans: Vec<(usize, usize, String)> = Vec::new();
    {
        let mut search = html.as_str();
        let mut offset = 0usize;
        for (level, _text, line) in &scanned.headings {
            let open = format!("<h{level}>");
            let Some(spos) = search.find(&open) else {
                break;
            };
            let after = &search[spos + open.len()..];
            let end_tag = format!("</h{level}>");
            let Some(e) = after.find(&end_tag) else { break };
            let inner = &after[..e];
            let text = strip_tags(inner);
            let id = slugify(&text);
            let replacement =
                format!("<h{level} id=\"{id}\" data-line=\"{line}\">{inner}</h{level}>");
            let adv = spos + open.len() + inner.len() + end_tag.len();
            spans.push((offset + spos, offset + adv, replacement));
            search = &after[e + end_tag.len()..];
            offset += adv;
        }
    }
    {
        let mut rebuilt = String::with_capacity(html.len() + spans.len() * 32);
        let mut pos = 0usize;
        for (a, b, rep) in spans {
            rebuilt.push_str(&html[pos..a]);
            rebuilt.push_str(&rep);
            pos = b;
        }
        rebuilt.push_str(&html[pos..]);
        html = rebuilt;
    }
    // 2. Code fences: highlight with the class mapper.
    html = highlight_fences(&html, &scanned.fences);
    // 3. Alerts: blockquote starting with [!X].
    html = render_alerts(&html);
    // 4. Suggestion blocks (needs context).
    if let Some(c) = ctx {
        html = render_suggestions(&html, c);
    }
    // 5. Links and images.
    html = rewrite_links_images(&html, base_dir, raw_base, source);
    // 6. data-line on remaining block tags, in document order.
    add_data_lines(html, scanned)
}

/// Replace <pre><code class?..> bodies with highlighted spans.
fn highlight_fences(html: &str, fences: &[(String, String, usize)]) -> String {
    use crate::hl::{find_syntax, highlight_line, syntax_set};
    use syntect::parsing::{ParseState, ScopeStack};
    let ss = syntax_set();
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    let mut fi = fences.iter();
    loop {
        let Some(pre) = rest.find("<pre>") else {
            out.push_str(rest);
            break;
        };
        // find matching </pre>
        let Some(end) = rest.find("</pre>") else {
            out.push_str(rest);
            break;
        };
        let chunk = &rest[pre..end + "</pre>".len()];
        let (lang, _text, line) = fi.next().cloned().unwrap_or_default();
        // Extract code text between <code...> and </code>. `head` keeps the
        // <code …> opener only (the <pre> is re-emitted with data-line).
        let (head, code, tail) = match chunk.find("<code") {
            Some(cs) => {
                let gt = chunk[cs..]
                    .find('>')
                    .map(|i| cs + i + 1)
                    .unwrap_or(chunk.len());
                let ce = chunk.find("</code>").unwrap_or(chunk.len());
                (&chunk[cs..gt], &chunk[gt..ce], &chunk[ce..])
            }
            None => {
                out.push_str(&rest[..end + "</pre>".len()]);
                rest = &rest[end + "</pre>".len()..];
                continue;
            }
        };
        let syntax = if lang.is_empty() {
            ss.find_syntax_plain_text()
        } else {
            find_syntax(ss, Some(lang.as_str()), None)
        };
        let mut parse = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        let mut body = String::new();
        for raw in code.split('\n') {
            // code HTML is escaped; unescape before highlighting.
            body.push_str(&highlight_line(
                &html_unescape(raw),
                ss,
                &mut parse,
                &mut stack,
            ));
            body.push('\n');
        }
        if body.ends_with('\n') {
            body.pop();
        }
        out.push_str(&rest[..pre]);
        out.push_str(&format!("<pre data-line=\"{line}\">{head}{body}{tail}"));
        rest = &rest[end + "</pre>".len()..];
    }
    out
}

fn render_alerts(html: &str) -> String {
    // pulldown-cmark renders `> [!NOTE]` natively as
    // `<blockquote class="markdown-alert-note">` (marker already stripped).
    // Map to the contract classes; leave plain blockquotes alone.
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    loop {
        let Some(bq) = rest.find("<blockquote") else {
            out.push_str(rest);
            break;
        };
        let gt = rest[bq..].find('>').map(|i| bq + i);
        let Some(gt) = gt else {
            out.push_str(rest);
            break;
        };
        let open = &rest[bq..=gt];
        let kind = ["note", "tip", "important", "warning", "caution"]
            .iter()
            .find_map(|k| open.contains(&format!("markdown-alert-{k}")).then_some(*k));
        let Some(end_rel) = rest[gt..].find("</blockquote>") else {
            out.push_str(rest);
            break;
        };
        let end = gt + end_rel + "</blockquote>".len();
        match kind {
            Some(k) => {
                let inner = &rest[gt + 1..gt + end_rel];
                out.push_str(&rest[..bq]);
                out.push_str(&format!(
                    "<div class=\"md-alert md-alert-{k}\">{inner}</div>"
                ));
            }
            None => out.push_str(&rest[..end]),
        }
        rest = &rest[end..];
    }
    out
}

fn render_suggestions(html: &str, ctx: &RenderContextDoc) -> String {
    // ```suggestion blocks are fenced with language "suggestion".
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    loop {
        let Some(pre) = rest.find("<pre>") else {
            out.push_str(rest);
            break;
        };
        let Some(end) = rest.find("</pre>") else {
            out.push_str(rest);
            break;
        };
        let chunk = &rest[pre..end + "</pre>".len()];
        let is_sug = chunk.contains("language-suggestion");
        out.push_str(&rest[..pre]);
        if !is_sug {
            out.push_str(chunk);
        } else {
            let code = chunk
                .split_once("<code")
                .and_then(|(_, r)| r.find('>').map(|i| &r[i + 1..]))
                .and_then(|r| r.split_once("</code>").map(|(c, _)| c))
                .unwrap_or("");
            let sug = html_unescape(code);
            let header = match (&ctx.path, ctx.start_line, ctx.end_line) {
                (Some(p), Some(a), Some(b)) => format!("Suggested change for {p}:{a}-{b}"),
                (Some(p), _, _) => format!("Suggested change for {p}"),
                _ => "Suggested change".to_string(),
            };
            // Added-lines preview of the suggestion.
            let mut rows = String::new();
            for l in sug.lines() {
                rows.push_str(&format!("<div class=\"sug-add\">+{}</div>", esc(l)));
            }
            out.push_str(&format!(
                "<div class=\"md-suggestion\"><div class=\"sug-head\">{}</div>{rows}</div>",
                esc(&header)
            ));
        }
        rest = &rest[end + "</pre>".len()..];
    }
    out
}

fn rewrite_links_images(html: &str, base_dir: &str, raw_base: &str, _source: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    // Rewrite <a href> and <img src> occurrences in order.
    loop {
        let ia = rest.find("<a ");
        let ii = rest.find("<img");
        let (pos, is_img) = match (ia, ii) {
            (Some(a), Some(i)) => {
                if a < i {
                    (a, false)
                } else {
                    (i, true)
                }
            }
            (Some(a), None) => (a, false),
            (None, Some(i)) => (i, true),
            (None, None) => break,
        };
        out.push_str(&rest[..pos]);
        if !is_img {
            let gt = rest[pos..].find('>').map(|i| i + pos);
            let Some(gt) = gt else {
                out.push_str(&rest[pos..]);
                break;
            };
            let mut tag = rest[pos..=gt].to_string();
            let href = attr_val(&tag, "href");
            match href {
                Some(h)
                    if h.starts_with("http://")
                        || h.starts_with("https://")
                        || h.starts_with("mailto:") =>
                {
                    if !tag.contains("target=") {
                        tag = tag.replacen("<a ", "<a target=\"_blank\" ", 1);
                    }
                    out.push_str(&tag);
                }
                Some(h) if h.contains(':') => {
                    // Not http(s)/mailto but has a scheme (javascript:, data:, …): drop it.
                    tag = remove_attr(&tag, "href");
                    out.push_str(&tag);
                }
                Some(h_raw) => {
                    // Attribute text is HTML-escaped by the renderer: unescape once here,
                    // escape once on output (escaping it again produced `&amp;amp;`).
                    let h = html_unescape(&h_raw);
                    let (path_part, frag) = match h.split_once('#') {
                        Some((p, f)) => (p, Some(f)),
                        None => (h.as_str(), None),
                    };
                    let title = attr_val(&tag, "title")
                        .map(|t| format!(" title=\"{t}\""))
                        .unwrap_or_default();
                    if path_part.is_empty() {
                        // Same-document anchor: headings carry matching ids, so keep it.
                        out.push_str(&format!("<a href=\"#{}\"{title}>", esc(frag.unwrap_or(""))));
                    } else if let Some(data_path) =
                        join_workspace(base_dir, &percent_decode(path_part))
                    {
                        // Workspace link, resolved against the md file's dir (`..` included).
                        let mut open = format!("<a href=\"#\" data-path=\"{}\"", esc(&data_path));
                        // `#L12` or `#L12-L20`: the first line number.
                        if let Some(line) = frag
                            .and_then(|f| f.strip_prefix('L'))
                            .and_then(|n| n.split(|c: char| !c.is_ascii_digit()).next())
                            .and_then(|n| n.parse::<usize>().ok())
                            .filter(|&n| n > 0)
                        {
                            open.push_str(&format!(" data-line=\"{line}\""));
                        }
                        open.push_str(&title);
                        open.push('>');
                        out.push_str(&open);
                    } else {
                        // Climbs above the workspace root: keep the text, drop the link.
                        tag = remove_attr(&tag, "href");
                        out.push_str(&tag);
                    }
                }
                None => out.push_str(&tag),
            }
            rest = &rest[gt + 1..];
        } else {
            let gt = rest[pos..].find('>').map(|i| i + pos);
            let Some(gt) = gt else {
                out.push_str(&rest[pos..]);
                break;
            };
            let mut tag = rest[pos..=gt].to_string();
            // `src_raw` is the escaped attribute text (used to locate it in the tag);
            // `src` is the real value (escaped exactly once on output).
            let src_raw = attr_val(&tag, "src");
            let src = src_raw.as_deref().map(html_unescape);
            match src.as_deref() {
                Some(src) if src.starts_with("http://") || src.starts_with("https://") => {
                    // External: no src, only data-ext-src (opt-in rendering).
                    tag = remove_attr(&tag, "src");
                    tag = tag.replacen("<img", &format!("<img data-ext-src=\"{}\"", esc(src)), 1);
                    out.push_str(&tag);
                }
                Some(src) if !src.contains(':') => {
                    match join_workspace(base_dir, &percent_decode(src)) {
                        Some(joined) => {
                            tag = tag.replacen(
                                &format!("src=\"{}\"", src_raw.as_deref().unwrap_or_default()),
                                &format!(
                                    "src=\"{raw_base}/api/v1/file/raw?path={}\"",
                                    esc(&query_encode(&joined))
                                ),
                                1,
                            );
                        }
                        // Climbs above the workspace root.
                        None => tag = remove_attr(&tag, "src"),
                    }
                    out.push_str(&tag);
                }
                _ => {
                    tag = remove_attr(&tag, "src");
                    out.push_str(&tag);
                }
            }
            rest = &rest[gt + 1..];
        }
    }
    out.push_str(rest);
    out
}

fn attr_val(tag: &str, name: &str) -> Option<String> {
    let pat = format!("{name}=\"");
    let s = tag.find(&pat)? + pat.len();
    let e = tag[s..].find('"')?;
    Some(tag[s..s + e].to_string())
}

fn remove_attr(tag: &str, name: &str) -> String {
    let pat = format!(" {name}=\"");
    match tag.find(&pat) {
        Some(s) => {
            let rest = &tag[s + pat.len()..];
            match rest.find('"') {
                Some(e) => format!("{}{}", &tag[..s], &rest[e + 1..]),
                None => tag.to_string(),
            }
        }
        None => tag.to_string(),
    }
}

/// Pair block tags with scanned source lines, in document order.
/// Pair remaining block tags with scanned source lines, in document order.
/// Headings and code blocks already carry data-line; `<pre>` is skipped.
fn add_data_lines(mut html: String, scanned: &DocBlocks) -> String {
    for (b, line) in &scanned.blocks {
        // (needle, replacement); blockquote may carry the alert class already.
        let (needle, attr) = match b {
            Block::Heading(_) | Block::Pre => continue,
            Block::Para => ("<p>", format!("<p data-line=\"{line}\">")),
            Block::Quote => ("<blockquote", format!("<blockquote data-line=\"{line}\"")),
            Block::List(false) => ("<ul>", format!("<ul data-line=\"{line}\">")),
            Block::List(true) => ("<ol>", format!("<ol data-line=\"{line}\">")),
            Block::Item => ("<li>", format!("<li data-line=\"{line}\">")),
            Block::Table => ("<table>", format!("<table data-line=\"{line}\">")),
            Block::Hr => ("<hr />", format!("<hr data-line=\"{line}\" />")),
        };
        // Consume one occurrence so identical tags pair in order.
        let Some(p) = html.find(needle) else { continue };
        let mut rebuilt = String::with_capacity(html.len() + 16);
        rebuilt.push_str(&html[..p]);
        rebuilt.push_str(&attr);
        rebuilt.push_str(&html[p + needle.len()..]);
        html = rebuilt;
    }
    html
}

/// Ammonia gate for v2 HTML: § 5.4 allowlist incl. data-*, heading ids,
/// target=_blank, alert/suggestion divs, checkbox inputs, th/td align.
pub fn sanitize_v2(html: &str) -> String {
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
    ammonia::Builder::default()
        .tags(tags)
        .url_schemes(["http", "https", "mailto"].into_iter().collect())
        .add_tag_attributes("a", &["href", "title", "target", "data-path", "data-line"])
        .add_tag_attributes(
            "img",
            &["src", "alt", "title", "width", "height", "data-ext-src"],
        )
        .add_tag_attributes("input", &["type", "checked", "disabled"])
        .add_tag_attributes("th", &["align"])
        .add_tag_attributes("td", &["align"])
        .add_tag_attributes("code", &["class"])
        .add_tag_attributes("span", &["class"])
        .add_tag_attributes("div", &["class"])
        .add_tag_attributes("h1", &["id", "data-line"])
        .add_tag_attributes("h2", &["id", "data-line"])
        .add_tag_attributes("h3", &["id", "data-line"])
        .add_tag_attributes("h4", &["id", "data-line"])
        .add_tag_attributes("h5", &["id", "data-line"])
        .add_tag_attributes("h6", &["id", "data-line"])
        .add_tag_attributes("p", &["data-line"])
        .add_tag_attributes("pre", &["data-line"])
        .add_tag_attributes("ul", &["data-line"])
        .add_tag_attributes("ol", &["data-line"])
        .add_tag_attributes("li", &["data-line"])
        .add_tag_attributes("blockquote", &["data-line"])
        .add_tag_attributes("table", &["data-line"])
        .add_tag_attributes("hr", &["data-line"])
        .attribute_filter(|element, attribute, value| {
            let ok = match (element, attribute) {
                (e, "id") if e.len() == 2 && e.starts_with('h') => true,
                (_, "id") => false,
                ("a", "href") => {
                    value.starts_with("http://")
                        || value.starts_with("https://")
                        || value.starts_with("mailto:")
                        // Same-document anchors (`#install`, `#%C3%A9t%C3%A9`).
                        || value.strip_prefix('#').is_some_and(|f| {
                            f.chars().all(|c| c.is_alphanumeric() || "-_.%~".contains(c))
                        })
                }
                ("a", "target") => value == "_blank",
                ("a", "data-path") | ("a", "data-line") => true,
                ("img", "src") => {
                    !(value.contains(':')
                        && !value.starts_with("https://")
                        && !value.starts_with("http://"))
                }
                ("img", "data-ext-src") => {
                    value.starts_with("https://") || value.starts_with("http://")
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
                // Highlight spans inside code blocks (§ 11 closed set).
                ("span", "class") => value.split_whitespace().all(|c| {
                    c.len() >= 3
                        && c.starts_with("t-")
                        && c[2..].chars().all(|x| x.is_ascii_lowercase() || x == '-')
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
                (_, "data-line") => value.parse::<usize>().map(|n| n > 0).unwrap_or(false),
                _ => true,
            };
            ok.then_some(Cow::Borrowed(value))
        })
        .clean(html)
        .to_string()
}

async fn file_markdown(
    State(s): State<Arc<AppState>>,
    Query(q): Query<MdQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let abs = super::files::resolve_pub(&s, &q.path)?;
    if abs.metadata().map(|m| !m.is_file()).unwrap_or(true) {
        return Err(ApiError::not_found(format!("not found: {}", q.path)));
    }
    let size = abs.metadata().map(|m| m.len()).unwrap_or(0);
    if size > s.limits.max_markdown_bytes {
        return Err(ApiError::new(
            crate::error::ErrorCode::TooLarge,
            "file over maxMarkdownBytes",
        ));
    }
    let text = tokio::task::spawn_blocking(move || std::fs::read_to_string(&abs))
        .await
        .map_err(|_| ApiError::new(crate::error::ErrorCode::Internal, "read task failed"))?
        .map_err(|_| ApiError::new(crate::error::ErrorCode::Unsupported, "not UTF-8 text"))?;
    // Workspace-relative dir of the file for link targets.
    let dir = q
        .path
        .rsplit_once('/')
        .map(|(d, _)| d.to_string())
        .unwrap_or_default();
    let r = render_v2(&text, &dir, "", None);
    Ok(Json(serde_json::json!({
        "html": r.html,
        "headings": r.headings.iter().map(|(level, text, id, line)| {
            serde_json::json!({ "level": level, "text": text, "id": id, "line": line })
        }).collect::<Vec<_>>(),
        "externalImages": r.external_images,
    })))
}

async fn render(
    State(s): State<Arc<AppState>>,
    body: AxumBytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.len() > 1024 * 1024 {
        return Err(ApiError::new(
            crate::error::ErrorCode::TooLarge,
            "body over 1 MiB",
        ));
    }
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
    let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let path = v
        .get("path")
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .to_string();
    let ctx = v.get("context").map(|c| RenderContextDoc {
        path: c
            .get("path")
            .and_then(|p| p.as_str())
            .map(|x| x.to_string()),
        start_line: c
            .get("startLine")
            .and_then(|n| n.as_u64())
            .map(|n| n as usize),
        end_line: c
            .get("endLine")
            .and_then(|n| n.as_u64())
            .map(|n| n as usize),
    });
    let dir = path
        .rsplit_once('/')
        .map(|(d, _)| d.to_string())
        .unwrap_or_default();
    let _ = s;
    let r = render_v2(text, &dir, "", ctx.as_ref());
    Ok(Json(serde_json::json!({
        "html": r.html,
        "headings": r.headings.iter().map(|(level, text, id, line)| {
            serde_json::json!({ "level": level, "text": text, "id": id, "line": line })
        }).collect::<Vec<_>>(),
        "externalImages": r.external_images,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_doc(src: &str) -> Rendered {
        render_v2(src, "docs", "", None)
    }

    #[test]
    fn headings_ids_and_lines() {
        let r = render_doc("# Hello World!\n\nSome text.\n\n## Sub §ection\n");
        assert!(
            r.html.contains("<h1 id=\"hello-world\" data-line=\"1\">"),
            "{}",
            r.html
        );
        assert!(
            r.html.contains("<h2 id=\"sub-ection\" data-line=\"5\">"),
            "{}",
            r.html
        );
        assert_eq!(r.headings.len(), 2);
        assert_eq!(r.headings[0].2, "hello-world");
    }

    #[test]
    fn fences_highlighted_with_classes() {
        let r = render_doc("```rust\nfn main() {}\n```\n");
        assert!(r.html.contains("class=\"t-k\""), "{}", r.html);
        assert!(!r.html.contains("style="), "{}", r.html);
    }

    #[test]
    fn alerts_and_suggestion() {
        let r = render_doc("> [!NOTE]\n> take note\n");
        assert!(r.html.contains("md-alert md-alert-note"), "{}", r.html);
        assert!(!r.html.contains("[!NOTE]"), "{}", r.html);
    }

    #[test]
    fn links_and_images_rewritten() {
        let r = render_doc("[ext](https://x.example/a) [ws](./other.md) [frag](./a.md#L10)\n\n![e](https://x.example/i.png) ![w](./i.png)\n");
        assert!(r.html.contains("target=\"_blank\""), "{}", r.html);
        assert!(r.html.contains("data-path=\"docs/other.md\""), "{}", r.html);
        assert!(
            r.html.contains("data-ext-src=\"https://x.example/i.png\""),
            "{}",
            r.html
        );
        assert!(
            !r.html.contains("<img src=\"https://x.example/i.png\""),
            "{}",
            r.html
        );
        assert!(
            r.html.contains("api/v1/file/raw?path=docs/i.png"),
            "{}",
            r.html
        );
        assert_eq!(r.external_images, 1);
    }

    #[test]
    fn xss_stays_out() {
        let r = render_doc("# T\n\n<img src=x onerror=alert(1)>\n\n[evil](javascript:alert(2))\n\n<script>alert(3)</script>\n");
        assert!(!r.html.contains("alert("));
        assert!(!r.html.contains("javascript:"));
        assert!(!r.html.contains("<script"));
    }

    #[test]
    fn data_lines_nondecreasing() {
        let src = "# H\n\npara one\n\n- a\n- b\n\n```py\nx = 1\n```\n\n> quote here\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        let r = render_doc(src);
        let mut lines: Vec<usize> = Vec::new();
        let mut rest = r.html.as_str();
        while let Some(p) = rest.find("data-line=\"") {
            let tail = &rest[p + 11..];
            let e = tail.find('"').unwrap();
            lines.push(tail[..e].parse().unwrap());
            rest = &tail[e..];
        }
        assert!(!lines.is_empty());
        assert!(lines.windows(2).all(|w| w[0] <= w[1]), "{lines:?}");
    }

    // ---- review fixes (2026-09-25) ----

    #[test]
    fn external_image_count_ignores_text_and_code() {
        let r = render_doc("Images wait in `data-ext-src` until opted in.\n\n```html\n<img data-ext-src=\"https://x.test/a.png\">\n```\n\ndata-ext-src again\n");
        assert_eq!(r.external_images, 0, "{}", r.html);
        let r = render_doc("![a](https://x.test/a.png) and `data-ext-src`\n");
        assert_eq!(r.external_images, 1, "{}", r.html);
    }

    #[test]
    fn parent_links_resolve_and_never_escape() {
        let r = render_v2(
            "[brand](../BRAND.md) [top](/README.md#L3-L9) [out](../../../etc/passwd) ![i](../img/a.png)",
            "docs/spec",
            "",
            None,
        );
        assert!(r.html.contains("data-path=\"docs/BRAND.md\""), "{}", r.html);
        assert!(
            r.html.contains("data-path=\"README.md\" data-line=\"3\""),
            "{}",
            r.html
        );
        assert!(!r.html.contains("etc/passwd"), "{}", r.html);
        assert!(!r.html.contains(".."), "{}", r.html);
        assert!(
            r.html.contains("file/raw?path=docs/img/a.png"),
            "{}",
            r.html
        );
    }

    #[test]
    fn same_document_anchors_survive() {
        let r = render_doc("[Install](#install) [bad](#a:b)\n\n## Install\n");
        assert!(r.html.contains("<a href=\"#install\""), "{}", r.html);
        assert!(r.html.contains("<h2 id=\"install\""), "{}", r.html);
        assert!(!r.html.contains("href=\"#a:b\""), "{}", r.html);
    }

    #[test]
    fn attribute_values_are_escaped_once() {
        let r = render_doc("![b](https://x.test/i?a=1&b=2) [w](my%20file&x.md) ![l](a&b.png)\n");
        assert!(
            r.html
                .contains("data-ext-src=\"https://x.test/i?a=1&amp;b=2\""),
            "{}",
            r.html
        );
        assert!(!r.html.contains("&amp;amp;"), "{}", r.html);
        assert!(
            r.html.contains("data-path=\"docs/my file&amp;x.md\""),
            "{}",
            r.html
        );
        assert!(r.html.contains("path=docs/a%26b.png"), "{}", r.html);
    }

    #[test]
    fn unescape_is_single_pass() {
        assert_eq!(html_unescape("&amp;lt;br&amp;gt;"), "&lt;br&gt;");
        assert_eq!(html_unescape("a &lt; b &amp;&amp; c"), "a < b && c");
        let r = render_doc("# Use `&lt;br&gt;`\n");
        assert_eq!(r.headings[0].1, "Use &lt;br&gt;");
    }

    #[test]
    fn workspace_path_helpers() {
        assert_eq!(
            join_workspace("docs/spec", "../a.md").as_deref(),
            Some("docs/a.md")
        );
        assert_eq!(
            join_workspace("docs", "./x/./y.md").as_deref(),
            Some("docs/x/y.md")
        );
        assert_eq!(
            join_workspace("docs", "/root.md").as_deref(),
            Some("root.md")
        );
        assert_eq!(join_workspace("docs", "../../x"), None);
        assert_eq!(join_workspace("", ".."), None);
        assert_eq!(percent_decode("my%20file%E2%9C%93.md"), "my file✓.md");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(query_encode("a b&c/ü.png"), "a%20b%26c/%C3%BC.png");
    }
}
