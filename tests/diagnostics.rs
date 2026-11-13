use cyallo::{compile, diagnostics::render};
#[test]
fn named_primary_and_related_labels() {
    let source = "const name = 1\nname = 2\n";
    let error = compile(source).unwrap_err();
    let rendered = render("immutable.ya", source, &error);
    assert!(rendered.contains("E107"), "{rendered}");
    assert!(rendered.contains("immutable.ya"), "{rendered}");
    assert!(rendered.contains("const name = 1"), "{rendered}");
    assert!(rendered.contains("name = 2"), "{rendered}");
    assert!(rendered.contains("cannot modify"), "{rendered}");
    assert!(!rendered.contains('\u{1b}'));
}
#[test]
fn unicode_tabs_and_crlf() {
    let source = "\tstd::io::print(\"é水🐈\", missing)\r\n";
    let error = compile(source).unwrap_err();
    assert_eq!(error.span.source_text(source), "missing");
    let rendered = render("unicode.ya", source, &error);
    assert!(rendered.contains("é水🐈"), "{rendered}");
    assert!(rendered.contains("missing"), "{rendered}");
    assert!(rendered.contains("unicode.ya"), "{rendered}");
    assert!(!rendered.contains("out of bounds"), "{rendered}");
}
#[test]
fn eof_label_renders() {
    let source = "if true {\n";
    let error = compile(source).unwrap_err();
    assert_eq!(error.span.start as usize, source.len());
    let rendered = render("eof.ya", source, &error);
    assert!(rendered.contains("expected }"), "{rendered}");
    assert!(rendered.contains("eof.ya"), "{rendered}");
}
#[test]
fn diagnostic_color_is_explicitly_selectable() {
    let source = "std::io::print(unknown)\n";
    let diagnostic = compile(source).unwrap_err();
    let colored = cyallo::diagnostics::render_with_color("color.ya", source, &diagnostic, true);
    let plain = cyallo::diagnostics::render_with_color("color.ya", source, &diagnostic, false);
    assert!(colored.contains('\u{1b}'));
    assert!(!plain.contains('\u{1b}'));
    assert!(colored.contains("color.ya"));
    assert!(plain.contains("E106"));
}
