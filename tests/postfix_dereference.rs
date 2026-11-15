use cyallo::backend::{
    CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend, Tier, TieredBackend,
};
use cyallo::{CompileOptions, Context, Session, Status, compile, compile_for_execution};
use cyallo_ast::{Expr, ExprKind, Operator, StmtKind};
use std::{sync::Arc, time::Duration};

fn parity(source: &str, expected: &str) {
    for deferred in [false, true] {
        let program = Arc::new(
            if deferred {
                compile_for_execution(source, CompileOptions::default())
            } else {
                compile(source)
            }
            .unwrap(),
        );
        let winch = TieredBackend::default().compile(program.clone()).unwrap();
        for function in 0..program.functions().len() {
            winch.precompile(function, Tier::Winch).unwrap();
        }
        assert!(winch.wait_for_compilation(Duration::from_secs(30)));
        let engines: Vec<Option<Box<dyn Executable>>> = vec![
            None,
            Some(Box::new(winch)),
            Some(Box::new(CraneliftBackend.compile(program.clone()).unwrap())),
            Some(Box::new(
                CraneliftSpeedBackend.compile(program.clone()).unwrap(),
            )),
        ];
        for engine in engines {
            let mut session = Session::with_context(
                program.clone(),
                Context {
                    gc: cyallo::runtime::gc::GcConfig::stress(),
                    ..Default::default()
                },
            )
            .unwrap();
            for step in 0..10_000 {
                match &engine {
                    Some(engine) if step % 3 != 0 => engine.run(&mut session, 1).unwrap(),
                    _ => {
                        session.run(1);
                    }
                }
                if session.status() != &Status::Paused {
                    break;
                }
            }
            assert_eq!(session.status(), &Status::Finished);
            assert_eq!(session.context.output, expected);
        }
    }
}

fn initializer(source: &str) -> Expr {
    let ast = cyallo_parser::parse(source).unwrap();
    let StmtKind::Binding(_, _, _, _, value) = &ast.body[0].kind else {
        panic!("expected binding")
    };
    value.clone()
}

#[test]
fn postfix_dereference_composes_with_references_managed_owners_and_cells() {
    parity(
        include_str!("../examples/postfix-dereference.ya"),
        "21\n12\n7\n",
    );
}

#[test]
fn dereference_chains_preserve_index_tuple_call_cast_and_multiplication_precedence() {
    parity(
        r#"
        let mut values = [2, 4]
        let view = &mut values
        view.*[1] += 3
        std::io::print(view.*[0] * view.*[1])
        let references = [&values[0], &values[1]]
        std::io::print(references[0].* + references[1].*)
        let tuple = (4, 6)
        let tuple_view = &tuple
        std::io::print(tuple_view.*.0 + tuple_view.*.1)
        std::io::print((gc 8).* as u64 * 2 as u64)
        std::io::print(rc(5).* * 4)
        let cell = Cell::new(10)
        cell.* = cell.* * 2
        std::io::print(cell.*)
    "#,
        "14\n9\n10\n16\n20\n20\n",
    );
}

#[test]
fn dereferenced_assignment_targets_are_evaluated_once() {
    parity(
        r#"
        let mut calls = 0
        let counter = gc Cell::new(1)
        let counters = [counter]
        proc index() -> i64 { calls += 1; return 0 }
        proc target() -> gc Cell<i64> { calls += 1; return counter }
        counters[index()].*.* = 42
        std::io::print(calls, ":", counter.*.*)
        std::io::print(target().*.*)
        let mut numbers = [4]
        let reference = &mut numbers[index()]
        reference.* += 3
        std::io::print(calls, ":", reference.*)
    "#,
        "1:42\n42\n3:7\n",
    );
}

#[test]
fn parser_keeps_postfix_dereference_above_prefix_operators_and_binary_arithmetic() {
    let expression = initializer("let result = -items[1].* * factory<i64>().*.0 as i64");
    let ExprKind::Binary(Operator::Multiply, left, right) = expression.kind else {
        panic!("expected multiplication")
    };
    assert!(
        matches!(&left.kind, ExprKind::Unary(Operator::Subtract, operand) if matches!(operand.kind, ExprKind::Unary(Operator::Dereference, _)))
    );
    assert!(
        matches!(&right.kind, ExprKind::Cast(value, _) if matches!(&value.kind, ExprKind::TupleField(base, 0) if matches!(base.kind, ExprKind::Unary(Operator::Dereference, _))))
    );
    assert!(
        matches!(initializer("let result = &mut value.*").kind, ExprKind::Unary(Operator::BorrowMut, value) if matches!(value.kind, ExprKind::Unary(Operator::Dereference, _)))
    );
    assert!(
        matches!(initializer("let result = (&mut value).*").kind, ExprKind::Unary(Operator::Dereference, value) if matches!(value.kind, ExprKind::Parenthesized(_)))
    );
    assert!(
        matches!(initializer("let result = rc(5) * 4").kind, ExprKind::Binary(Operator::Multiply, value, _) if matches!(value.kind, ExprKind::ManagedNew(_, _, None, _)))
    );
}

#[test]
fn lexer_spans_and_highlighter_preserve_chained_dereference_after_tuple_indices() {
    let source = "// 水\nlet value = values.0.*.1.* * 2\n";
    let value = initializer(source);
    assert_eq!(
        &source[value.span.start as usize..value.span.end as usize],
        "values.0.*.1.* * 2"
    );
    let colored = cyallo::highlight::highlight(source, true).unwrap();
    let mut plain = String::new();
    let mut chars = colored.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    assert_eq!(plain, source);
    let error = compile("let invalid = 42.*").unwrap_err();
    assert_eq!(error.code, "E109");
    assert_eq!(Operator::Dereference.to_string(), ".*");
}

#[test]
fn prefix_dereference_has_a_migration_diagnostic() {
    for source in [
        "let n = *value",
        "let n = **value",
        "let n = &*value",
        "proc update(value: &mut i64) { *value = 1 }",
    ] {
        let error = compile(source).unwrap_err();
        assert_eq!(error.code, "E180", "{source}: {error}");
        assert_eq!(
            &source[error.span.start as usize..error.span.end as usize],
            "*"
        );
    }
    assert_eq!(
        cyallo_ast::diagnostic::DereferenceSyntaxError::PrefixRemoved.to_string(),
        "use postfix dereference `value.*` instead of `*value`"
    );
}

#[test]
fn postfix_syntax_retains_readonly_and_cell_borrow_restrictions() {
    for source in [
        "let value = 1; let view = &value; view.* = 2",
        "let cell = Cell::new(1); let view = &cell.*",
        "let cell = Cell::new(1); let view = &mut cell.*",
        "let cell = Cell::new(1); cell.* += 1",
        "let cell = Cell::new(rc 1); let value = cell.*",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
}
