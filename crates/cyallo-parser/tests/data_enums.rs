use cyallo_ast::*;
use cyallo_parser::{index_modern, parse};

#[test]
fn declarations_keep_nominality_payloads_and_annotations() {
    let source = r#"
#[cyallo_internal("test_enum")]
pub open enum Outcome<T, E> {
    #[cyallo_internal("test_ok")] Ok: T,
    Err: E,
    Pending,
    Progress: record { total: i64, completed: i64 },
    Pair: (i64, string),
}
"#;
    let ast = parse(source).unwrap();
    assert!(ast.records.is_empty());
    let declaration = &ast.enums[0];
    assert_eq!(declaration.name.as_ref(), "Outcome");
    assert_eq!(declaration.type_params, [Box::<str>::from("T"), "E".into()]);
    assert!(declaration.open);
    assert_eq!(declaration.annotations[0].value.as_ref(), "test_enum");
    assert_eq!(
        declaration.variants[0].annotations[0].value.as_ref(),
        "test_ok"
    );
    assert_eq!(
        declaration.variants[0].payload,
        Some(Type::Named("T".into()))
    );
    assert!(declaration.variants[2].payload.is_none());
    let Some(Type::AnonymousRecord(fields)) = &declaration.variants[3].payload else {
        panic!("expected structural payload")
    };
    assert_eq!(
        fields
            .iter()
            .map(|(name, _)| name.as_ref())
            .collect::<Vec<_>>(),
        ["completed", "total"]
    );
    assert!(
        ast.exports
            .iter()
            .any(|name| name.as_ref() == "$type$Outcome")
    );
}

#[test]
fn match_keeps_nested_patterns_guards_blocks_and_tails() {
    let ast = parse(
        r#"
let answer = match event {
    Event::Pair((x, Event::Value(y))) | Event::Pair((y, Event::Value(x))) if x > 0 => {
        let sum = x + y
        sum
    },
    Event::Row { value: z, .. } => z,
    Event::Idle => 0,
    _ => -1,
}
"#,
    )
    .unwrap();
    let StmtKind::Binding(_, _, _, _, value) = &ast.body[0].kind else {
        panic!()
    };
    let ExprKind::Match(_, arms) = &value.kind else {
        panic!("expected match")
    };
    assert_eq!(arms.len(), 4);
    let PatternKind::Or(alternatives) = &arms[0].pattern.kind else {
        panic!()
    };
    assert_eq!(alternatives.len(), 2);
    assert!(arms[0].guard.is_some());
    let MatchBody::Block(statements, Some(tail)) = &arms[0].body else {
        panic!()
    };
    assert_eq!(statements.len(), 1);
    assert!(matches!(&tail.kind, ExprKind::Name(name) if name.as_ref()=="sum"));
    let PatternKind::Variant(_, Some(record)) = &arms[1].pattern.kind else {
        panic!()
    };
    assert!(matches!(record.kind, PatternKind::Record(_, true)));
    assert!(matches!(arms[3].pattern.kind, PatternKind::Wildcard));
}

#[test]
fn if_let_has_its_own_pattern_and_branches() {
    let ast = parse("if let Event::Row { value, .. } = event { emit(value) } else if let Event::Idle = event { idle() } else { other() }\n").unwrap();
    let StmtKind::IfLet(pattern, _, yes, no) = &ast.body[0].kind else {
        panic!()
    };
    assert!(matches!(pattern.kind, PatternKind::Variant(..)));
    assert_eq!(yes.len(), 1);
    assert!(matches!(no[0].kind, StmtKind::IfLet(..)));
}

#[test]
fn structural_type_order_is_canonical_but_literal_evaluation_order_is_preserved() {
    let ast = parse("type A = record { z: i64, a: string }\ntype B = record { a: string, z: i64 }\nlet value = record { z: first(), a: second() }\n").unwrap();
    assert_eq!(ast.type_aliases[0].ty, ast.type_aliases[1].ty);
    let StmtKind::Binding(_, _, _, _, value) = &ast.body[0].kind else {
        panic!()
    };
    let ExprKind::AnonymousRecord(fields) = &value.kind else {
        panic!()
    };
    assert_eq!(
        fields.iter().map(|(n, _)| n.as_ref()).collect::<Vec<_>>(),
        ["z", "a"]
    );
}

#[test]
fn parse_jobs_retain_every_enum_and_every_pattern_span() {
    let source = "enum A { One, Two: i64 }\nenum B { Empty, Value: record { n: i64 } }\nlet value = match item { B::Value { n } if n > 0 => { let x: i64 = n; x }, _ => 0 }\nif let A::Two(n) = other { emit(n) }\n";
    let index = index_modern(source).unwrap();
    let mut serial = index.parse(false, cyallo_compile_jobs::Jobs(1)).unwrap();
    let parallel = index.parse(false, cyallo_compile_jobs::Jobs(8)).unwrap();
    assert_eq!(format!("{serial:?}"), format!("{parallel:?}"));
    let mut before = vec![];
    cyallo_ast::spans::visit_spans(&mut serial, |span| before.push(*span));
    cyallo_ast::spans::shift_ast(&mut serial, 113).unwrap();
    let mut after = vec![];
    cyallo_ast::spans::visit_spans(&mut serial, |span| after.push(*span));
    assert_eq!(before.len(), after.len());
    assert!(before.len() > 25);
    for (old, new) in before.into_iter().zip(after) {
        assert_eq!(new, Span::new(old.start + 113, old.end + 113));
    }
}

#[test]
fn visitor_reaches_match_guards_tail_expressions_and_block_type_annotations() {
    let mut ast=parse("let value = match item { E::Value(x) if valid(x) => { let y: i64 = x; finish(y) }, _ => 0 }\n").unwrap();
    let mut calls = vec![];
    let mut types = vec![];
    cyallo_ast::visit::visit_block(
        &mut ast.body,
        &mut |expr| {
            if let ExprKind::Call(name, _) = &expr.kind {
                calls.push(name.clone());
            }
            Ok(())
        },
        &mut |ty, _| {
            types.push(ty.clone());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(calls, [Box::<str>::from("valid"), "finish".into()]);
    assert_eq!(types, [Type::Integer]);
}

#[test]
fn duplicate_payload_fields_variants_and_malformed_patterns_are_diagnostics() {
    for source in [
        "enum E { A, A }\n",
        "enum E { A: record { x: i64, x: i64 } }\n",
        "let x = record { a: 1, a: 2 }\n",
        "let x = match value { E::A { x, x } => 0 }\n",
    ] {
        let error = parse(source).unwrap_err();
        assert_eq!(error.code, "E183", "{error:?}");
        assert!(error.span.end > error.span.start);
    }
    for source in [
        "enum E { A: }\n",
        "let x = match value { E::A(x) -> x }\n",
        "if let = x { f() }\n",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
}

#[test]
fn empty_enum_and_empty_match_remain_representable_for_the_checker() {
    let ast = parse("enum Never {}\nlet x = match never {}\n").unwrap();
    assert!(ast.enums[0].variants.is_empty());
    let StmtKind::Binding(_, _, _, _, value) = &ast.body[0].kind else {
        panic!()
    };
    assert!(matches!(&value.kind,ExprKind::Match(_,arms) if arms.is_empty()));
}

#[test]
fn pattern_recursion_obeys_the_parser_depth_limit() {
    let source = format!(
        "let result = match x {{ {}_{} => 0 }}\n",
        "(".repeat(300),
        ")".repeat(300)
    );
    let error = parse(&source).unwrap_err();
    assert_eq!(error.code, "E002");
    assert!(error.message.contains("nesting exceeds"));
}

#[test]
fn match_is_also_an_expression_statement_and_open_is_contextual() {
    let ast =
        parse("proc open() {}\nopen()\nmatch event { E::A => run(), _ => other() }\n").unwrap();
    assert!(matches!(
        &ast.body[1].kind,
        StmtKind::Evaluate(Expr {
            kind: ExprKind::Match(..),
            ..
        })
    ));
}

#[test]
fn open_variant_version_annotations_are_retained() {
    let ast = parse("pub open enum Failure { #[since(\"1.2\")] Denied: string, Other }\n").unwrap();
    let annotation = &ast.enums[0].variants[0].annotations[0];
    assert_eq!(annotation.name.as_ref(), "since");
    assert_eq!(annotation.value.as_ref(), "1.2");
}

#[test]
fn typed_syntax_failures_retain_payloads_and_have_separate_presentation() {
    use cyallo_ast::diagnostic::{EnumSyntaxError, invalid_enum_syntax};
    let error = EnumSyntaxError::DuplicateVariant {
        name: "Value".into(),
    };
    assert!(matches!(&error,EnumSyntaxError::DuplicateVariant {name} if name.as_ref()=="Value"));
    assert_eq!(error.to_string(), "duplicate enum variant `Value`");
    let diagnostic = invalid_enum_syntax(error, Span::new(5, 10));
    assert_eq!(diagnostic.span, Span::new(5, 10));
    assert_eq!(diagnostic.code, "E183");
}
