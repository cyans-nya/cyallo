use cyallo::{SourceFile, Span};
use cyallo_ast::{ExprKind, GetSpan, StmtKind};
#[test]
fn tokens_slice_exact_utf8_source() {
    let source = "// 注釈\r\nOUTPUT \"é🐈\", '水' ← 12\r\n";
    let tokens = caie_int_parser::lex(source).unwrap();
    let slices: Vec<_> = tokens
        .iter()
        .map(|t| &source[t.span.start as usize..t.span.end as usize])
        .collect();
    assert_eq!(
        slices,
        vec!["\n", "OUTPUT", "\"é🐈\"", ",", "'水'", "←", "12", "\n", ""]
    );
    for t in tokens {
        assert!(source.is_char_boundary(t.span.start as usize));
        assert!(source.is_char_boundary(t.span.end as usize));
    }
}
#[test]
fn complete_expression_and_statement_ranges() {
    let source = "  OUTPUT LEFT(\"é水\", 1) & \"!\" // comment\n";
    let ast = caie_int_parser::parse(source).unwrap();
    let statement = &ast.body[0];
    assert_eq!(
        &source[statement.span().start as usize..statement.span().end as usize],
        "OUTPUT LEFT(\"é水\", 1) & \"!\""
    );
    let StmtKind::Output(es) = &statement.kind else {
        panic!()
    };
    assert_eq!(
        &source[es[0].span.start as usize..es[0].span.end as usize],
        "LEFT(\"é水\", 1) & \"!\""
    );
    let ExprKind::Binary(_, a, b) = &es[0].kind else {
        panic!()
    };
    assert_eq!(
        &source[a.span.start as usize..a.span.end as usize],
        "LEFT(\"é水\", 1)"
    );
    assert_eq!(&source[b.span.start as usize..b.span.end as usize], "\"!\"");
}
#[test]
fn compound_nodes_include_end_keyword() {
    let source = "IF TRUE THEN\n OUTPUT 1\nELSE\n OUTPUT 2\nENDIF\n";
    let ast = caie_int_parser::parse(source).unwrap();
    assert_eq!(&source[..ast.body[0].span.end as usize], source.trim_end());
}
#[test]
fn source_locations_and_eof() {
    let source = SourceFile::new("é🐈\r\nx\n".into()).unwrap();
    assert_eq!(source.location(0), Some((1, 1)));
    assert_eq!(source.location(2), Some((1, 2)));
    assert_eq!(source.location(1), None);
    assert_eq!(source.location(8), Some((2, 1)));
    assert_eq!(source.location(10), Some((3, 1)));
    assert_eq!(source.location(11), None);
    assert_eq!(source.slice(Span::new(0, 2)), Some("é"));
    assert_eq!(source.slice(Span::new(1, 2)), None);
    assert_eq!(std::mem::size_of::<Span>(), 8);
}
#[test]
fn lexical_and_parse_errors_have_real_ranges() {
    for source in [
        "OUTPUT \"é\n",
        "OUTPUT 'ab'\n",
        "OUTPUT @\n",
        "OUTPUT (1 + )\n",
    ] {
        let d = caie_int_parser::parse(source).unwrap_err();
        assert!(d.span.start <= d.span.end);
        assert!(d.span.end as usize <= source.len());
        assert!(source.is_char_boundary(d.span.start as usize));
        assert!(source.is_char_boundary(d.span.end as usize));
    }
    let d = caie_int_parser::parse("IF TRUE THEN\n").unwrap_err();
    assert_eq!(d.span, Span::empty(13));
}
#[test]
fn parentheses_preserve_inner_ranges() {
    let source = "OUTPUT (1 + (2 * 3))\n";
    let ast = caie_int_parser::parse(source).unwrap();
    let StmtKind::Output(es) = &ast.body[0].kind else {
        panic!()
    };
    let ExprKind::Parenthesized(inner) = &es[0].kind else {
        panic!()
    };
    assert_eq!(es[0].span.source_text(source), "(1 + (2 * 3))");
    assert_eq!(inner.span.source_text(source), "1 + (2 * 3)");
    let ExprKind::Binary(_, _, rhs) = &inner.kind else {
        panic!()
    };
    let ExprKind::Parenthesized(inner) = &rhs.kind else {
        panic!()
    };
    assert_eq!(inner.span.source_text(source), "2 * 3");
}
#[test]
fn parameter_span_includes_passing_mode() {
    let source = "PROCEDURE P(BYREF x : INTEGER, y : INTEGER)\nENDPROCEDURE\n";
    let ast = caie_int_parser::parse(source).unwrap();
    assert_eq!(
        ast.functions[0].params[0].span.source_text(source),
        "BYREF x : INTEGER"
    );
    assert_eq!(
        ast.functions[0].params[1].span.source_text(source),
        "y : INTEGER"
    );
}
#[test]
fn duplicate_declaration_labels_both_locations() {
    let source = "DECLARE name : INTEGER\nDECLARE NAME : INTEGER\n";
    let d = cyallo::compile_with_options(
        source,
        cyallo::CompileOptions {
            dialect: cyallo::Dialect::Compatibility,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(d.span.source_text(source), "DECLARE NAME : INTEGER");
    assert_eq!(d.related.len(), 1);
    assert_eq!(d.related[0].0.source_text(source), "DECLARE name : INTEGER");
}
