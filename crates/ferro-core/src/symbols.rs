//! Tree-sitter outline (B6): per-language tags queries → `SymbolKind`.
//! Each supported grammar ships its own `tags.scm`; `@definition.*` captures
//! give the symbol span and `@name` the identifier. Languages without a
//! `ts-*` feature fall back to the regex extractor (`source: "regex"`).
//!
//! Default features cover Rust, Go, TypeScript/TSX, JavaScript, Python,
//! Java, C, Ruby, and PHP. C++ (`ts-cpp`) and C# (`ts-csharp`) stay opt-in:
//! their parsers add ~9 MB linked, which would breach the +10 MiB binary
//! budget (BACKEND.md B6); both fall back to the regex extractor.

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: &'static str,
    pub line: usize,
    pub end_line: usize,
    pub depth: usize,
    /// First source line of the definition, trimmed (hover signatures).
    pub detail: String,
}

/// Extension → supported grammar. Returns false when the `ts-*` feature is
/// off, so callers fall back to regex.
pub fn supported(ext: &str) -> bool {
    language_for(ext).is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Cpp/CSharp only constructed with ts-cpp/ts-csharp.
enum Lang {
    Rust,
    Go,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Java,
    C,
    Cpp,
    CSharp,
    Ruby,
    Php,
}

fn language_for(ext: &str) -> Option<Lang> {
    match ext {
        #[cfg(feature = "ts-rust")]
        "rs" => Some(Lang::Rust),
        #[cfg(feature = "ts-go")]
        "go" => Some(Lang::Go),
        #[cfg(feature = "ts-typescript")]
        "ts" | "mts" | "cts" => Some(Lang::TypeScript),
        #[cfg(feature = "ts-typescript")]
        "tsx" => Some(Lang::Tsx),
        #[cfg(feature = "ts-javascript")]
        "js" | "jsx" | "mjs" | "cjs" => Some(Lang::JavaScript),
        #[cfg(feature = "ts-python")]
        "py" => Some(Lang::Python),
        #[cfg(feature = "ts-java")]
        "java" => Some(Lang::Java),
        #[cfg(feature = "ts-c")]
        "c" => Some(Lang::C),
        #[cfg(feature = "ts-cpp")]
        "h" | "hpp" | "cc" | "cxx" | "cpp" => Some(Lang::Cpp),
        #[cfg(feature = "ts-csharp")]
        "cs" => Some(Lang::CSharp),
        #[cfg(feature = "ts-ruby")]
        "rb" => Some(Lang::Ruby),
        #[cfg(feature = "ts-php")]
        "php" => Some(Lang::Php),
        _ => None,
    }
}

/// Tree-sitter outline. `None` = unsupported extension or parse failure
/// (caller uses regex). Caps at 5000 symbols like the regex extractor.
pub fn outline_ts(ext: &str, text: &str) -> Option<Vec<Symbol>> {
    let lang = language_for(ext)?;
    outline_lang(lang, text)
}

#[cfg(feature = "tree-sitter")]
fn outline_lang(lang: Lang, text: &str) -> Option<Vec<Symbol>> {
    use std::collections::HashSet;
    use streaming_iterator::StreamingIterator;
    use tree_sitter::{Parser, QueryCursor};

    if text.len() > 8 * 1024 * 1024 {
        return None;
    }
    let (language, tags) = grammar(lang)?;
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(text, None)?;
    let root = tree.root_node();
    if root.has_error() && root.child(0).is_none() {
        return None;
    }
    let query = cached_query(lang, &language, &tags)?;
    let names = query.capture_names();
    let bytes = text.as_bytes();
    let mut cursor = QueryCursor::new();
    // Collect byte spans first (nodes borrow the tree); kind/depth/detail
    // resolve below against the live tree.
    let mut hits: Vec<(usize, usize, usize, usize, String)> = Vec::new();
    let mut matches = cursor.matches(query, root, bytes);
    loop {
        matches.advance();
        let Some(m) = matches.get() else { break };
        let mut name_span = None;
        let mut def_span = None;
        let mut suffix = String::new();
        for cap in m.captures {
            let cname = names.get(cap.index as usize).copied().unwrap_or("");
            if cname == "name" && name_span.is_none() {
                let n = cap.node;
                name_span = Some((n.start_byte(), n.end_byte()));
            } else if let Some(s) = cname.strip_prefix("definition.") {
                if def_span.is_none() {
                    let d = cap.node;
                    def_span = Some((d.start_byte(), d.end_byte()));
                    suffix = s.to_string();
                }
            }
        }
        if let (Some((ns, ne)), Some((ds, de))) = (name_span, def_span) {
            hits.push((ns, ne, ds, de, suffix));
        }
    }
    // Byte ranges back to names + def spans resolved on the live tree.
    let mut def_ids: HashSet<usize> = HashSet::new();
    for (_, _, ds, de, _) in &hits {
        if let Some(n) = node_at(root, *ds, *de) {
            def_ids.insert(n.id());
        }
    }
    let mut out = Vec::new();
    for (ns, ne, ds, de, suffix) in hits {
        let name = text.get(ns..ne).unwrap_or("").trim().to_string();
        if name.is_empty() || name.len() > 256 || name.chars().any(|c| c.is_whitespace()) {
            continue;
        }
        let def_node = match node_at(root, ds, de) {
            Some(n) => n,
            None => continue,
        };
        let kind = map_kind(lang, &suffix, def_node);
        let line = def_node.start_position().row + 1;
        let end_line = def_node.end_position().row + 1;
        let mut depth = 0usize;
        let mut p = def_node.parent();
        while let Some(n) = p {
            // Definitions nest; Rust `impl` blocks scope methods without
            // being symbols themselves.
            if def_ids.contains(&n.id()) || (lang == Lang::Rust && n.kind() == "impl_item") {
                depth += 1;
            }
            p = n.parent();
        }
        let detail = text
            .get(ds..de)
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(200)
            .collect::<String>();
        out.push(Symbol {
            name,
            kind,
            line,
            end_line,
            depth,
            detail,
        });
        if out.len() >= 5000 {
            break;
        }
    }
    out.sort_by_key(|s| (s.line, s.name.clone()));
    Some(out)
}

/// Find the definition node by its exact span: the smallest node covering
/// the full capture span is the definition itself.
#[cfg(feature = "tree-sitter")]
fn node_at<'t>(
    root: tree_sitter::Node<'t>,
    start: usize,
    end: usize,
) -> Option<tree_sitter::Node<'t>> {
    root.descendant_for_byte_range(start, end)
}

/// Tags-query `@definition.*` suffix + def-node kind → `SymbolKind`.
#[cfg(feature = "tree-sitter")]
fn map_kind(lang: Lang, suffix: &str, def: tree_sitter::Node<'_>) -> &'static str {
    let dk = def.kind();
    match lang {
        Lang::Rust => match dk {
            "struct_item" => "struct",
            "enum_item" => "enum",
            "union_item" => "type",
            "trait_item" => "trait",
            "type_item" => "type",
            "mod_item" => "module",
            "macro_definition" | "macro_rules!" => "macro",
            "function_item" => {
                if def.parent().map(|p| p.kind()) == Some("declaration_list") {
                    "method"
                } else {
                    "function"
                }
            }
            _ => match suffix {
                "function" => "function",
                "method" => "method",
                "class" => "class",
                "interface" => "interface",
                "module" => "module",
                "macro" => "macro",
                _ => "other",
            },
        },
        Lang::Go => {
            if suffix == "type" {
                return "type";
            }
            match suffix {
                "function" => "function",
                "method" => "method",
                _ => "other",
            }
        }
        _ => match suffix {
            "function" => "function",
            "method" => "method",
            "class" => "class",
            "interface" => "interface",
            "module" => "module",
            "macro" => "macro",
            "type" => "type",
            "constant" => "const",
            _ => "other",
        },
    }
}

/// Grammar + bundled tags query per language (feature-gated).
/// TypeScript's bundled query only covers signatures (ambient/overload
/// contexts), so plain `function f() {}` declarations get a documented
/// supplement in the same capture vocabulary.
#[cfg(feature = "tree-sitter")]
fn grammar(lang: Lang) -> Option<(tree_sitter::Language, std::borrow::Cow<'static, str>)> {
    use std::borrow::Cow;
    Some(match lang {
        #[cfg(feature = "ts-rust")]
        Lang::Rust => (
            tree_sitter_rust::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_rust::TAGS_QUERY),
        ),
        #[cfg(feature = "ts-go")]
        Lang::Go => (
            tree_sitter_go::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_go::TAGS_QUERY),
        ),
        #[cfg(feature = "ts-typescript")]
        Lang::TypeScript => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Cow::Owned(format!(
                "{}\n{TS_SUPPLEMENT}",
                tree_sitter_typescript::TAGS_QUERY
            )),
        ),
        #[cfg(feature = "ts-typescript")]
        Lang::Tsx => (
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            Cow::Owned(format!(
                "{}\n{TS_SUPPLEMENT}",
                tree_sitter_typescript::TAGS_QUERY
            )),
        ),
        #[cfg(feature = "ts-javascript")]
        Lang::JavaScript => (
            tree_sitter_javascript::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_javascript::TAGS_QUERY),
        ),
        #[cfg(feature = "ts-python")]
        Lang::Python => (
            tree_sitter_python::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_python::TAGS_QUERY),
        ),
        #[cfg(feature = "ts-java")]
        Lang::Java => (
            tree_sitter_java::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_java::TAGS_QUERY),
        ),
        #[cfg(feature = "ts-c")]
        Lang::C => (
            tree_sitter_c::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_c::TAGS_QUERY),
        ),
        #[cfg(feature = "ts-cpp")]
        Lang::Cpp => (
            tree_sitter_cpp::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_cpp::TAGS_QUERY),
        ),
        #[cfg(feature = "ts-csharp")]
        Lang::CSharp => (
            tree_sitter_c_sharp::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_c_sharp::TAGS_QUERY),
        ),
        #[cfg(feature = "ts-ruby")]
        Lang::Ruby => (
            tree_sitter_ruby::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_ruby::TAGS_QUERY),
        ),
        #[cfg(feature = "ts-php")]
        Lang::Php => (
            tree_sitter_php::LANGUAGE_PHP.into(),
            Cow::Borrowed(tree_sitter_php::TAGS_QUERY),
        ),
        #[allow(unreachable_patterns)]
        _ => return None,
    })
}

#[cfg(feature = "ts-typescript")]
const TS_SUPPLEMENT: &str = r#"
(function_declaration
  name: (identifier) @name) @definition.function

(generator_function_declaration
  name: (identifier) @name) @definition.function

(class_declaration
  name: (type_identifier) @name) @definition.class

(method_definition
  name: (property_identifier) @name) @definition.method
"#;
/// One compiled tags query per language, shared across threads.
#[cfg(feature = "tree-sitter")]
fn cached_query(
    lang: Lang,
    language: &tree_sitter::Language,
    tags: &str,
) -> Option<&'static tree_sitter::Query> {
    use std::sync::OnceLock;
    macro_rules! slot {
        ($name:ident) => {{
            static $name: OnceLock<tree_sitter::Query> = OnceLock::new();
            $name.get_or_init(|| {
                tree_sitter::Query::new(language, tags).expect("tags query compiles")
            })
        }};
    }
    Some(match lang {
        Lang::Rust => slot!(Q_RUST),
        Lang::Go => slot!(Q_GO),
        Lang::TypeScript => slot!(Q_TS),
        Lang::Tsx => slot!(Q_TSX),
        Lang::JavaScript => slot!(Q_JS),
        Lang::Python => slot!(Q_PY),
        Lang::Java => slot!(Q_JAVA),
        Lang::C => slot!(Q_C),
        Lang::Cpp => slot!(Q_CPP),
        Lang::CSharp => slot!(Q_CS),
        Lang::Ruby => slot!(Q_RB),
        Lang::Php => slot!(Q_PHP),
    })
}

#[cfg(not(feature = "tree-sitter"))]
fn outline_lang(_lang: Lang, _text: &str) -> Option<Vec<Symbol>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbols_for(ext: &str, text: &str) -> Vec<Symbol> {
        outline_ts(ext, text).unwrap_or_default()
    }

    #[test]
    fn rust_struct_enum_fn() {
        let syms = symbols_for("rs", "pub struct Foo;\n\nenum Bar { A }\n\nfn top() {}\n\nimpl Foo {\n    fn meth(&self) {}\n}\n");
        if !cfg!(feature = "ts-rust") {
            return;
        }
        let kinds: Vec<(&str, &str)> = syms.iter().map(|s| (s.name.as_str(), s.kind)).collect();
        assert!(kinds.contains(&("Foo", "struct")), "{kinds:?}");
        assert!(kinds.contains(&("Bar", "enum")), "{kinds:?}");
        assert!(kinds.contains(&("top", "function")), "{kinds:?}");
        assert!(kinds.contains(&("meth", "method")), "{kinds:?}");
        let meth = syms.iter().find(|s| s.name == "meth").unwrap();
        assert!(meth.depth >= 1, "{meth:?}");
        assert!(meth.end_line >= meth.line);
    }

    #[test]
    fn python_nesting() {
        let syms = symbols_for("py", "class A:\n    def f(self):\n        pass\ndef g():\n");
        if !cfg!(feature = "ts-python") {
            return;
        }
        let f = syms.iter().find(|s| s.name == "f").unwrap();
        assert_eq!(f.kind, "function");
        assert!(f.depth >= 1);
        assert!(syms.iter().any(|s| s.name == "A" && s.kind == "class"));
    }

    #[test]
    fn go_fixture() {
        let syms = symbols_for(
            "go",
            "package main\n\ntype S struct{}\n\nfunc top() {}\n\nfunc (s S) meth() {}\n",
        );
        if !cfg!(feature = "ts-go") {
            return;
        }
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"top"), "{names:?}");
        assert!(names.contains(&"meth"), "{names:?}");
    }

    #[test]
    fn unsupported_falls_back() {
        assert!(outline_ts("xyz", "nothing").is_none());
        assert!(!supported("xyz"));
        assert!(supported("rs") == cfg!(feature = "ts-rust"));
    }

    /// Every default grammar parses its smoke sample without panicking and
    /// yields at least the headline symbol. Guards per-feature builds too.
    #[test]
    fn all_grammars_smoke() {
        let cases: &[(&str, &str, &str, &str)] = &[
            (
                "js",
                "function hi() {}\nclass K {}\n",
                "ts-javascript",
                "hi",
            ),
            (
                "ts",
                "function hi(): void {}\ninterface I {}\n",
                "ts-typescript",
                "hi",
            ),
            (
                "tsx",
                "export function App() { return null; }\n",
                "ts-typescript",
                "App",
            ),
            ("java", "class A {\n void m() {}\n}\n", "ts-java", "A"),
            ("c", "int main(void) {\n}\n", "ts-c", "main"),
            ("cpp", "class A {\n void m() {}\n};\n", "ts-cpp", "A"),
            ("h", "int f(void);\n", "ts-cpp", "f"),
            ("cs", "class A {\n void M() {}\n}\n", "ts-csharp", "A"),
            ("rb", "class Foo\n def bar?\n end\nend\n", "ts-ruby", "Foo"),
            ("php", "<?php\nfunction hi() {}\n", "ts-php", "hi"),
        ];
        for (ext, text, feature, want) in cases {
            let syms = symbols_for(ext, text);
            if syms.is_empty() {
                // Feature off in this build: must report unsupported so the
                // endpoint falls back to regex.
                assert!(
                    !supported(ext),
                    "{ext} ({feature}) empty but claims support"
                );
                continue;
            }
            let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
            assert!(names.contains(want), "{ext} ({feature}): {names:?}");
            for s in &syms {
                assert!(s.end_line >= s.line, "{s:?}");
                assert!(!s.name.is_empty() && s.name.len() <= 256, "{s:?}");
            }
        }
    }
}
