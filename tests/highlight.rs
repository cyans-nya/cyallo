fn highlight(source: &str, color: bool) -> Result<String, cyallo::Diagnostic> {
    cyallo::highlight::highlight_with_dialect(source, color, cyallo::Dialect::Compatibility)
}
fn plain(ansi: &str) -> String {
    let mut out = String::new();
    let mut chars = ansi.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            assert_eq!(chars.next(), Some('['));
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn assert_style(colored: &str, text: &str, style: &str) {
    assert!(
        colored.contains(&format!("\x1b[{style}m{text}\x1b[0m")),
        "expected {text:?} in style {style:?}: {colored:?}"
    );
}
#[test]
fn coloring_preserves_source_exactly() {
    for source in [
        "",
        "// comment only",
        "\tDECLARE n : INTEGER\r\nn ← 123 // hello\r\nOUTPUT \"é水🐈 // string\", '水', MID(\"abc\",1,2)",
        "FUNCTION F() RETURNS INTEGER\n RETURN 1\nENDFUNCTION\n",
    ] {
        let colored = highlight(source, true).unwrap();
        assert_eq!(plain(&colored), source);
        assert_eq!(highlight(source, false).unwrap(), source);
    }
}
#[test]
fn comments_and_literals_have_distinct_styles() {
    let result = highlight("OUTPUT \"// literal\" // comment\n", true).unwrap();
    assert!(result.contains("\x1b[32m\"// literal\"\x1b[0m"));
    assert!(result.contains("\x1b[90m// comment\x1b[0m\n"));
    assert!(result.contains("\x1b[1;35mOUTPUT\x1b[0m"));
}
#[test]
fn highlighting_does_not_require_a_valid_program() {
    assert!(highlight("missing <- unknown + 123\n", true).is_ok());
    assert!(highlight("OUTPUT \"unterminated", true).is_err());
}
#[test]
fn compatibility_keeps_literal_backslashes() {
    let source = concat!(r"OUTPUT '\'", "\n");
    assert!(
        cyallo::compile_with_options(
            source,
            cyallo::CompileOptions {
                dialect: cyallo::Dialect::Compatibility,
                ..Default::default()
            }
        )
        .is_ok()
    );
    assert!(
        cyallo::highlight::highlight_with_dialect(source, true, cyallo::Dialect::Compatibility)
            .is_ok()
    );
}

#[test]
fn modern_imports_and_mutability_are_colored() {
    let source = "use std::io;\nlet mut value = 1;\n";
    let colored = cyallo::highlight::highlight(source, true).unwrap();
    assert_eq!(plain(&colored), source);
    for keyword in ["use", "let", "mut"] {
        assert_style(&colored, keyword, "1;35");
    }
}

#[test]
fn modern_contextual_keywords_cover_declarations_and_modifiers() {
    for (source, keywords) in [
        ("pub(crate) mod shapes;", vec!["pub", "crate", "mod"]),
        ("pub use std::io::print as write;", vec!["pub", "use", "as"]),
        ("pub trait Build {}", vec!["pub", "trait"]),
        ("impl Build for Item {}", vec!["impl", "for"]),
        ("pub property Item.value: i64 {}", vec!["pub", "property"]),
        (
            "fn work(read value: &mut Item) -> i64;",
            vec!["fn", "read", "mut"],
        ),
        ("fn work(value: &'a mut Item) -> i64;", vec!["fn", "mut"]),
        ("let value = @local gc Item {}", vec!["let", "local", "gc"]),
        (
            "let value = @shared rc Item {}",
            vec!["let", "shared", "rc"],
        ),
        ("yield from sequence()", vec!["yield", "from"]),
        ("value as dyn Build", vec!["as", "dyn"]),
    ] {
        let colored = cyallo::highlight::highlight(source, true).unwrap();
        assert_eq!(plain(&colored), source);
        for keyword in keywords {
            assert_style(&colored, keyword, "1;35");
        }
    }
}

#[test]
fn modern_styles_separate_names_literals_operators_and_punctuation() {
    let source = concat!(
        "#[inline(always)]\n",
        "pub mod shapes;\n",
        "record Item { value: i64 }\n",
        "const limit = 12u8\n",
        "fn build(value: f32) -> bool { return true }\n",
        "std::io::print(\"water 水 // literal\", '水', 1.25, false)\n",
        "let value = item.*.value + limit // comment\n",
    );
    let colored = cyallo::highlight::highlight(source, true).unwrap();
    assert_eq!(plain(&colored), source);
    for (style, names) in [
        ("95", vec!["#", "inline"]),
        (
            "1;35",
            vec!["pub", "mod", "record", "const", "fn", "return", "let"],
        ),
        ("1;34", vec!["Item", "i64", "f32", "bool"]),
        ("34", vec!["shapes", "std", "io"]),
        ("36", vec!["build", "print"]),
        ("1;33", vec!["limit"]),
        ("33", vec!["12u8", "1.25", "true", "false"]),
        ("32", vec!["\"water 水 // literal\"", "'水'"]),
        ("91", vec!["=", "->", "*", "+"]),
        ("90", vec!["(", ")", "{", "}", ":", ".", "// comment"]),
    ] {
        for name in names {
            assert_style(&colored, name, style);
        }
    }
}

#[test]
fn contextual_spellings_in_names_keep_their_role() {
    for (source, name, style) in [
        ("fn use() -> i64 { return 1 }", "use", "36"),
        ("fn mut() -> i64 { return 1 }", "mut", "36"),
        ("record trait {}", "trait", "1;34"),
        ("mod use;", "use", "34"),
        ("std::gc::share(value)", "gc", "34"),
        ("std::string::upper(value)", "string", "34"),
        ("std::collections::Vec::from(1..5)", "from", "36"),
        ("object.use()", "use", "36"),
    ] {
        let colored = cyallo::highlight::highlight(source, true).unwrap();
        assert_style(&colored, name, style);
        assert_eq!(plain(&colored), source);
    }
    for (source, name) in [
        ("let use = 1", "use"),
        ("let mut use = 1", "use"),
        ("object.mut", "mut"),
        ("fn f(use: i64) -> i64;", "use"),
    ] {
        let colored = cyallo::highlight::highlight(source, true).unwrap();
        assert!(
            !colored.contains(&format!("\x1b[1;35m{name}\x1b[0m")),
            "a name was colored as a keyword: {colored:?}"
        );
    }
}

#[test]
fn modern_coloring_is_lossless_for_real_sources_and_partial_input() {
    for source in [
        "",
        "// use mut pub 水",
        "\tuse std::io;\r\nlet mut n = 123u16 // 水\r\n",
        "fn unfinished<T>(value: &'a mut T) ->",
        "let text = \"\\\" // use mut\"; let ch = '\\n'",
        include_str!("../examples/tour.ya"),
        include_str!("../examples/modern.ya"),
        include_str!("../examples/concurrency/generators/delegation.ya"),
        include_str!("../std.ya"),
    ] {
        let colored = cyallo::highlight::highlight(source, true).unwrap();
        assert_eq!(plain(&colored), source);
        assert_eq!(cyallo::highlight::highlight(source, false).unwrap(), source);
    }
}

#[test]
fn compatibility_types_literals_and_operators_use_the_new_palette() {
    let source =
        "DECLARE n : INTEGER\nCONSTANT limit = 3\nIF TRUE AND FALSE THEN\nOUTPUT n DIV 2\nENDIF\n";
    for dialect in [
        cyallo::Dialect::Compatibility,
        cyallo::Dialect::Strict,
        cyallo::Dialect::Simplified,
    ] {
        let colored = cyallo::highlight::highlight_with_dialect(source, true, dialect).unwrap();
        assert_eq!(plain(&colored), source);
        assert_style(&colored, "DECLARE", "1;35");
        assert_style(&colored, "INTEGER", "1;34");
        assert_style(&colored, "limit", "1;33");
        assert_style(&colored, "TRUE", "33");
        assert_style(&colored, "AND", "91");
        assert_style(&colored, "DIV", "91");
    }
}

#[test]
fn modern_contextual_keywords_do_not_leak_into_compatibility() {
    let source = "DECLARE use : INTEGER\nDECLARE mut : INTEGER\nOUTPUT use + mut\n";
    let colored = highlight(source, true).unwrap();
    assert_eq!(plain(&colored), source);
    for name in ["use", "mut"] {
        assert!(!colored.contains(&format!("\x1b[1;35m{name}\x1b[0m")));
    }
}

#[test]
fn pretty_cli_colors_modern_keywords_and_preserves_source_in_every_mode() {
    let file = "examples/concurrency/generators/delegation.ya";
    let source = std::fs::read_to_string(file).unwrap();
    for mode in ["always", "auto", "never"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_cyallo"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["pretty", file, "--color", mode])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(plain(&text), source);
        if mode == "always" {
            assert_style(&text, "use", "1;35");
            assert_style(&text, "mut", "1;35");
        } else {
            assert_eq!(text, source);
        }
    }
}

#[test]
fn diagnostic_excerpts_use_the_same_keyword_styles() {
    let source = "use std::io::print\nlet mut value = missing\n";
    let start = source.find("missing").unwrap() as u32;
    let diagnostic = cyallo::Diagnostic::new(
        "E106",
        "unknown name",
        cyallo::Span::new(start, start + "missing".len() as u32),
    );
    let colored = cyallo::diagnostics::render_with_color("snippet.ya", source, &diagnostic, true);
    assert_style(&colored, "use", "1;35");
    assert_style(&colored, "mut", "1;35");
}
