//! The endpoint index (routes.md) — parsed at compile time for read-only tests and guards.

use axum::http::Method;

const API_MD: &str = include_str!("routes.md");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteEntry {
    pub method: Method,
    pub path: String,
    pub mutating: bool,
}

/// Parsed once: the audit middleware consults this on every API request.
static ROUTES: std::sync::LazyLock<Vec<RouteEntry>> =
    std::sync::LazyLock::new(|| parse_section2(API_MD));

/// Every `(method, path)` pair from routes.md, with mutating vs read classification.
pub fn api_contract_routes() -> Vec<RouteEntry> {
    ROUTES.clone()
}

/// True when routes.md lists this method on a matching path as a mutation.
pub fn contract_is_mutating(method: &Method, path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    ROUTES
        .iter()
        .any(|r| r.mutating && r.method == *method && path_matches(&r.path, path))
}

/// True when routes.md lists this method on a matching path as read-only.
pub fn contract_is_read(method: &Method, path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    ROUTES
        .iter()
        .any(|r| !r.mutating && r.method == *method && path_matches(&r.path, path))
}

/// Legacy `/api/*` (non-v1) writes blocked in `--read-only` (the former legacy API).
pub fn legacy_is_mutating(method: &Method, path: &str) -> bool {
    if !path.starts_with("/api/") || path.starts_with("/api/v1/") {
        return false;
    }
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

pub fn read_only_blocks(method: &Method, path: &str) -> bool {
    if path == "/api/v1/auth/logout" {
        return false;
    }
    contract_is_mutating(method, path) || legacy_is_mutating(method, path)
}

fn path_matches(template: &str, path: &str) -> bool {
    let t: Vec<&str> = template.trim_end_matches('/').split('/').collect();
    let p: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    if t.len() != p.len() {
        return false;
    }
    t.iter()
        .zip(p.iter())
        .all(|(a, b)| a.starts_with('{') && a.ends_with('}') || a == b)
}

fn parse_section2(md: &str) -> Vec<RouteEntry> {
    let start = md
        .find("## Endpoint index")
        .expect("routes.md: endpoint index missing");
    let rest = &md[start..];
    let end = rest.find("\n---\n").unwrap_or(rest.len());
    let section = &rest[..end];
    let mut out = Vec::new();
    for line in section.lines() {
        let line = line.trim();
        if !line.starts_with('|') || line.contains("Method | Path") || line.starts_with("|---") {
            continue;
        }
        let cols: Vec<&str> = line.split('|').map(|c| c.trim()).collect();
        if cols.len() < 4 {
            continue;
        }
        let methods = cols[1];
        let paths_cell = cols[2];
        expand_row(methods, paths_cell, &mut out);
    }
    out
}

fn expand_row(methods: &str, paths_cell: &str, out: &mut Vec<RouteEntry>) {
    let methods = methods.trim();
    if methods.is_empty() || methods == "—" {
        return;
    }
    let read_methods: Vec<&str> = methods.split('/').map(str::trim).collect();
    let path_specs = split_paths(paths_cell);
    let mut base_prefix = String::from("/api/v1");
    for spec in path_specs {
        let paths = expand_path_spec(&spec, &mut base_prefix);
        for path in paths {
            for m in &read_methods {
                let method = parse_method(m);
                let mutating = is_mutating_method(m);
                out.push(RouteEntry {
                    method,
                    path: path.clone(),
                    mutating,
                });
            }
        }
    }
}

fn is_mutating_method(method_token: &str) -> bool {
    let m = method_token.to_ascii_uppercase();
    !matches!(m.as_str(), "GET" | "HEAD")
}

fn parse_method(token: &str) -> Method {
    match token.trim().to_ascii_uppercase().as_str() {
        "GET" => Method::GET,
        "POST" => Method::POST,
        "PUT" => Method::PUT,
        "PATCH" => Method::PATCH,
        "DELETE" => Method::DELETE,
        "HEAD" => Method::HEAD,
        other => panic!("unknown method in routes.md: {other}"),
    }
}

fn split_paths(cell: &str) -> Vec<String> {
    let mut specs = Vec::new();
    let mut cur = String::new();
    let mut in_tick = false;
    for ch in cell.chars() {
        match ch {
            '`' => in_tick = !in_tick,
            ',' if !in_tick => {
                let s = cur.trim();
                if !s.is_empty() {
                    specs.push(s.to_string());
                }
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    let s = cur.trim();
    if !s.is_empty() {
        specs.push(s.to_string());
    }
    specs
}

fn expand_path_spec(spec: &str, base_prefix: &mut String) -> Vec<String> {
    let spec = spec.split('(').next().unwrap_or(spec).trim();
    if spec.contains('[') {
        let base = spec.split('[').next().unwrap_or(spec).trim_end_matches('/');
        return vec![base.to_string(), format!("{base}/{{id}}")];
    }
    if spec.starts_with("/api/") || spec == "/mcp" {
        if let Some((parent, _)) = spec.rsplit_once('/') {
            *base_prefix = parent.to_string();
        }
        return vec![spec.to_string()];
    }
    if let Some(trimmed) = spec.strip_prefix('/') {
        // `/unstage` and `/dismiss` are siblings of the previous absolute path.
        if !trimmed.contains('/') {
            return vec![format!("{}/{}", base_prefix.trim_end_matches('/'), trimmed)];
        }
        // `/nav/references` is shorthand for `/api/v1/nav/references`.
        return vec![format!("/api/v1/{trimmed}")];
    }
    vec![spec.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_settings_put_as_mutating() {
        assert!(contract_is_mutating(&Method::PUT, "/api/v1/settings"));
        assert!(contract_is_read(&Method::GET, "/api/v1/settings"));
    }

    #[test]
    fn git_relative_paths_expand() {
        assert!(contract_is_mutating(&Method::POST, "/api/v1/git/stage"));
        assert!(contract_is_mutating(&Method::POST, "/api/v1/git/commit"));
    }

    #[test]
    fn harness_and_mcp_listed() {
        assert!(contract_is_mutating(&Method::POST, "/api/v1/harness/edit"));
        assert!(contract_is_mutating(
            &Method::POST,
            "/api/v1/harness/revert"
        ));
        assert!(contract_is_mutating(&Method::PUT, "/api/v1/harness"));
        assert!(contract_is_read(&Method::GET, "/api/v1/harness"));
        assert!(contract_is_mutating(&Method::POST, "/mcp"));
        assert!(contract_is_mutating(&Method::POST, "/api/v1/git/hunk"));
    }

    #[test]
    fn nav_shorthand_stays_under_api_v1() {
        assert!(contract_is_read(&Method::GET, "/api/v1/nav/definition"));
        assert!(contract_is_read(&Method::GET, "/api/v1/nav/references"));
        assert!(contract_is_read(&Method::GET, "/api/v1/nav/hover"));
        assert!(
            !api_contract_routes()
                .iter()
                .any(|r| r.path == "/api/v1/references" || r.path == "/api/v1/hover"),
            "relative /nav/* paths must not collapse to /api/v1/<leaf>"
        );
    }

    #[test]
    fn findings_dismiss_is_a_sibling() {
        assert!(contract_is_mutating(
            &Method::POST,
            "/api/v1/ai/findings/sample/dismiss"
        ));
        assert!(contract_is_mutating(
            &Method::POST,
            "/api/v1/ai/findings/sample/accept"
        ));
    }

    #[test]
    fn any_legacy_write_is_blocked() {
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert!(legacy_is_mutating(&method, "/api/not-yet-listed"));
            assert!(!legacy_is_mutating(&method, "/api/v1/not-yet-listed"));
        }
        assert!(!legacy_is_mutating(&Method::GET, "/api/stats"));
        assert!(!legacy_is_mutating(&Method::POST, "/health"));
    }

    #[test]
    fn every_mutating_pair_has_placeholder_match() {
        for r in api_contract_routes().iter().filter(|r| r.mutating) {
            let sample = r.path.replace("{id}", "sample");
            assert!(
                contract_is_mutating(&r.method, &sample),
                "{} {}",
                r.method,
                r.path
            );
        }
    }

    #[test]
    fn read_routes_are_get_or_head_only() {
        for r in api_contract_routes().iter().filter(|r| !r.mutating) {
            assert!(
                r.method == Method::GET || r.method == Method::HEAD,
                "{} {}",
                r.method,
                r.path
            );
        }
    }
}
