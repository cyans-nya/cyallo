use cya_ls::{
    document::Document,
    highlight::{Kind, Language, SyntaxHighlighter},
};

fn highlighted(language: Language, source: &str) -> Vec<(String, Kind)> {
    let mut document = Document::new(language, 1, source.into()).unwrap();
    let mut highlighter = SyntaxHighlighter::new().unwrap();
    highlighter
        .tokens(&mut document)
        .unwrap()
        .to_vec()
        .into_iter()
        .map(|token| {
            let start = document.lines.offset(source, token.start).unwrap();
            let end = document
                .lines
                .offset(
                    source,
                    lsp_types::Position::new(
                        token.start.line,
                        token.start.character + token.length,
                    ),
                )
                .unwrap();
            (source[start..end].into(), token.kind)
        })
        .collect()
}
fn contains(tokens: &[(String, Kind)], text: &str, kind: Kind) {
    assert!(
        tokens.iter().any(|(t, k)| t == text && *k == kind),
        "missing {text:?} as {kind:?}: {tokens:?}"
    );
}

#[test]
fn modern_roles_contextual_names_and_paths() {
    let source = concat!(
        "#[inline(always)]\npub mod shapes;\nrecord Item { value: i64 }\n",
        "const limit = 12u8\nfn use(read mut: &'a Item) -> bool { return true }\n",
        "let mut value = item.*.value + limit\nstd::gc::share(value)\nobject.use()\n",
        "use std::io::print as write;\n",
    );
    let tokens = highlighted(Language::Cyallo, source);
    for (text, kind) in [
        ("inline", Kind::Decorator),
        ("pub", Kind::Keyword),
        ("shapes", Kind::Namespace),
        ("Item", Kind::Type),
        ("i64", Kind::Type),
        ("limit", Kind::Constant),
        ("12u8", Kind::Number),
        ("use", Kind::Function),
        ("mut", Kind::Parameter),
        ("'a", Kind::TypeParameter),
        ("share", Kind::Function),
        ("gc", Kind::Namespace),
        ("true", Kind::Keyword),
        ("use", Kind::Method),
    ] {
        contains(&tokens, text, kind);
    }
}

#[test]
fn modern_enums_patterns_and_worlds_have_semantic_tokens() {
    let source = concat!(
        "pub open enum Event { Click: record { code: i64 }, Stop }\n",
        "let value = match event {\n",
        "    Event::Click { code: _code } if _code > 0 => _code,\n",
        "    Event::Click { .. } | Event::Stop => 0,\n",
        "}\n",
        "pub interface mod api { pub opaque record Context; }\n",
        "pub world Plugin { import api; export proc start(); }\n",
    );
    cyallo_parser::parse(source).unwrap();
    let tokens = highlighted(Language::Cyallo, source);
    for (text, kind) in [
        ("open", Kind::Keyword),
        ("enum", Kind::Keyword),
        ("Click", Kind::Constant),
        ("match", Kind::Keyword),
        ("code", Kind::Property),
        ("_code", Kind::Variable),
        ("=>", Kind::Operator),
        ("|", Kind::Operator),
        ("interface", Kind::Keyword),
        ("opaque", Kind::Keyword),
        ("world", Kind::Keyword),
        ("Plugin", Kind::Type),
        ("import", Kind::Keyword),
        ("export", Kind::Keyword),
    ] {
        contains(&tokens, text, kind);
    }
}

#[test]
fn modern_keywords_follow_their_syntactic_roles() {
    let source = concat!(
        "pub(crate) mod match;\n",
        "use std::{gc, rc};\n",
        "type Pointer = @local gc i64\n",
        "type Shared = @shared rc char\n",
        "type View = dyn Shape\n",
        "fn from(read read: string) -> bool { return true }\n",
        "let mut local = gen proc() -> i64 { yield from child }\n",
        "let impl = local\n",
        "type Alias = impl\n",
        "let spawn = from(\"\")\n",
    );
    cyallo_parser::parse(source).unwrap();
    let tokens = highlighted(Language::Cyallo, source);
    for (word, expected) in [
        ("match", &[Kind::Namespace][..]),
        ("gc", &[Kind::Namespace, Kind::Keyword][..]),
        ("rc", &[Kind::Namespace, Kind::Keyword][..]),
        (
            "local",
            &[Kind::Keyword, Kind::Variable, Kind::Variable][..],
        ),
        ("shared", &[Kind::Keyword][..]),
        ("dyn", &[Kind::Keyword][..]),
        ("impl", &[Kind::Variable, Kind::Type][..]),
        ("read", &[Kind::Keyword, Kind::Parameter][..]),
        ("from", &[Kind::Function, Kind::Keyword, Kind::Function][..]),
        ("char", &[Kind::Type][..]),
        ("string", &[Kind::Type][..]),
        ("spawn", &[Kind::Variable][..]),
    ] {
        let actual: Vec<_> = tokens
            .iter()
            .filter_map(|(text, kind)| (text == word).then_some(*kind))
            .collect();
        assert_eq!(actual, expected, "{word}");
    }
}

#[test]
fn caie_case_literals_and_operators() {
    let tokens = highlighted(
        Language::Caie,
        concat!(
            "dEcLaRe Count : INTEGER\nCONSTANT limit = 3\nCount ← 1\n",
            "IF TRUE AND FALSE THEN\nOUTPUT \"// literal\\\", '\\', Count DIV 2 // comment\nENDIF\n",
            "FUNCTION Twice(n : INTEGER) RETURNS INTEGER\nRETURN n * 2\nENDFUNCTION\n",
            "OUTPUT Twice(Count)\n",
        ),
    );
    for (text, kind) in [
        ("dEcLaRe", Kind::Keyword),
        ("INTEGER", Kind::Type),
        ("limit", Kind::Constant),
        ("←", Kind::Operator),
        ("AND", Kind::Operator),
        ("DIV", Kind::Operator),
        ("Twice", Kind::Function),
        ("n", Kind::Parameter),
        ("\"// literal\\\"", Kind::String),
        ("'\\'", Kind::String),
        ("// comment", Kind::Comment),
    ] {
        contains(&tokens, text, kind);
    }
    for unsupported in ["CLASS", "TYPE", "OPENFILE", "DATE", "SET"] {
        let tokens = highlighted(Language::Caie, &format!("{unsupported} Example\n"));
        assert!(
            !tokens
                .iter()
                .any(|(text, kind)| text == unsupported && *kind == Kind::Keyword)
        );
    }
}

#[test]
fn unfinished_literals_do_not_consume_following_lines() {
    for (language, source, following) in [
        (
            Language::Cyallo,
            "let text = \"水🐈 // unfinished\nlet next = 3\n",
            "let",
        ),
        (
            Language::Caie,
            "OUTPUT \"水🐈 // unfinished\nDECLARE next : INTEGER\n",
            "DECLARE",
        ),
    ] {
        let tokens = highlighted(language, source);
        contains(&tokens, "\"水🐈 // unfinished", Kind::String);
        contains(&tokens, following, Kind::Keyword);
        contains(&tokens, "next", Kind::Variable);
    }
}

#[test]
fn range_tokens_are_clipped_and_relative_to_document_origin() {
    use lsp_types::{Position as P, Range};
    let mut highlighter = SyntaxHighlighter::new().unwrap();
    let mut doc =
        Document::new(Language::Cyallo, 1, "// 🐈水\r\nlet next = 123\r\n".into()).unwrap();
    let tokens = highlighter
        .semantic_tokens(&mut doc, Some(Range::new(P::new(1, 12), P::new(1, 13))))
        .unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(
        (
            tokens[0].delta_line,
            tokens[0].delta_start,
            tokens[0].length
        ),
        (1, 12, 1)
    );
    assert!(
        highlighter
            .semantic_tokens(&mut doc, Some(Range::new(P::new(0, 4), P::new(0, 5))))
            .is_err()
    );
}

#[test]
fn supported_examples_and_standard_declarations_have_error_free_trees() {
    fn visit(
        path: &std::path::Path,
        highlighter: &mut SyntaxHighlighter,
        checked: &mut usize,
        errors: &mut Vec<String>,
    ) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.ends_with("examples/startup") {
                // Generated benchmark inputs repeat the same syntax thousands of times.
                continue;
            }
            if path.is_dir() {
                visit(&path, highlighter, checked, errors);
                continue;
            }
            let language = match path.extension().and_then(|s| s.to_str()) {
                Some("ya" | "yas") => Language::Cyallo,
                Some("psc" | "pss") => Language::Caie,
                _ => continue,
            };
            let source = std::fs::read_to_string(&path).unwrap();
            let supported = match language {
                Language::Cyallo => cyallo_parser::parse(&source).is_ok(),
                Language::Caie => caie_int_parser::parse(&source).is_ok(),
            };
            if !supported {
                continue;
            }
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&language.grammar()).unwrap();
            let tree = parser.parse(&source, None).unwrap();
            if tree.root_node().has_error() {
                fn first_error(node: tree_sitter::Node<'_>) -> tree_sitter::Node<'_> {
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        if child.has_error() {
                            return first_error(child);
                        }
                    }
                    node
                }
                let error = first_error(tree.root_node());
                errors.push(format!(
                    "{}:{}: {:?}",
                    path.strip_prefix(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
                        .unwrap()
                        .display(),
                    error.start_position().row + 1,
                    &source[error.byte_range()]
                ));
            }
            let mut doc = Document::new(language, 1, source).unwrap();
            assert!(
                !highlighter.tokens(&mut doc).unwrap().is_empty(),
                "{}",
                path.display()
            );
            *checked += 1;
        }
    }
    let mut checked = 0;
    let mut errors = vec![];
    visit(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
        &mut SyntaxHighlighter::new().unwrap(),
        &mut checked,
        &mut errors,
    );
    visit(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
        &mut SyntaxHighlighter::new().unwrap(),
        &mut checked,
        &mut errors,
    );
    assert!(
        errors.is_empty(),
        "{} sources with syntax errors:\n{}",
        errors.len(),
        errors.join("\n")
    );
    assert!(
        checked > 20,
        "only {checked} supported examples were exercised"
    );
}
