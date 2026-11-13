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
