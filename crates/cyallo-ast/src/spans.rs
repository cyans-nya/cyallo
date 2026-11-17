//! Checked relocation of every source coordinate owned by an AST.

use crate::*;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpanEndpoint {
    Start,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpanShiftError {
    InvalidRange {
        span: Span,
    },
    Overflow {
        span: Span,
        offset: u32,
        endpoint: SpanEndpoint,
    },
}
impl fmt::Display for SpanShiftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRange { span } => {
                write!(f, "invalid source span {}..{}", span.start, span.end)
            }
            Self::Overflow {
                span,
                offset,
                endpoint,
            } => write!(
                f,
                "offset {offset} overflows the {endpoint:?} of source span {}..{}",
                span.start, span.end
            ),
        }
    }
}
impl std::error::Error for SpanShiftError {}

/// Owns diagnostic-code mapping for source coordinate relocation failures.
pub fn invalid_span_shift(error: SpanShiftError) -> Diagnostic {
    let span = match error {
        SpanShiftError::InvalidRange { span } | SpanShiftError::Overflow { span, .. } => span,
    };
    Diagnostic::new("E175", error.to_string(), span)
}

fn shifted(span: Span, offset: u32) -> Result<Span, SpanShiftError> {
    if span.start > span.end {
        return Err(SpanShiftError::InvalidRange { span });
    }
    let start = span
        .start
        .checked_add(offset)
        .ok_or(SpanShiftError::Overflow {
            span,
            offset,
            endpoint: SpanEndpoint::Start,
        })?;
    let end = span
        .end
        .checked_add(offset)
        .ok_or(SpanShiftError::Overflow {
            span,
            offset,
            endpoint: SpanEndpoint::End,
        })?;
    Ok(Span::new(start, end))
}

/// Relocates the AST transactionally. Validation visits every coordinate before
/// any is changed, so overflow leaves the caller's AST unchanged.
pub fn shift_ast_checked(ast: &mut Ast, offset: u32) -> Result<(), SpanShiftError> {
    walk_ast(ast, &mut |span| shifted(*span, offset).map(|_| ()))?;
    walk_ast(ast, &mut |span| {
        *span = shifted(*span, offset)?;
        Ok(())
    })
}

/// Shifts all AST coordinates and presents typed failures as diagnostics.
pub fn shift_ast(ast: &mut Ast, offset: u32) -> Result<(), Diagnostic> {
    shift_ast_checked(ast, offset).map_err(invalid_span_shift)
}

/// Visit every coordinate, including declaration attributes, nested closures,
/// native headers, and related diagnostics. Source-map clients use this to
/// retain file-relative identities independently of archive placement.
pub fn visit_spans(ast: &mut Ast, mut visit: impl FnMut(&mut Span)) {
    walk_ast(ast, &mut |span| {
        visit(span);
        Ok(())
    })
    .expect("infallible span visitor");
}

/// Relocates primary and related diagnostic spans; leaves all unchanged on error.
pub fn shift_diagnostic(diagnostic: &mut Diagnostic, offset: u32) -> Result<(), Diagnostic> {
    walk_diagnostic(diagnostic, &mut |span| shifted(*span, offset).map(|_| ()))
        .map_err(invalid_span_shift)?;
    walk_diagnostic(diagnostic, &mut |span| {
        *span = shifted(*span, offset)?;
        Ok(())
    })
    .map_err(invalid_span_shift)
}

type VisitResult = Result<(), SpanShiftError>;

fn walk_diagnostic(
    diagnostic: &mut Diagnostic,
    visit: &mut impl FnMut(&mut Span) -> VisitResult,
) -> VisitResult {
    visit(&mut diagnostic.span)?;
    for (span, _) in &mut diagnostic.related {
        visit(span)?;
    }
    Ok(())
}
fn walk_annotations(
    annotations: &mut [DeclarationAnnotation],
    visit: &mut impl FnMut(&mut Span) -> VisitResult,
) -> VisitResult {
    for annotation in annotations {
        visit(&mut annotation.span)?;
    }
    Ok(())
}
fn walk_function(
    function: &mut Function,
    visit: &mut impl FnMut(&mut Span) -> VisitResult,
) -> VisitResult {
    crate::with_stack(|| {
        visit(&mut function.span)?;
        walk_annotations(&mut function.annotations, visit)?;
        for span in [
            &mut function.attributes.heat_span,
            &mut function.attributes.compilation_span,
            &mut function.attributes.inline_span,
        ]
        .into_iter()
        .flatten()
        {
            visit(span)?;
        }
        for parameter in &mut function.params {
            visit(&mut parameter.span)?;
        }
        walk_block(&mut function.body, visit)
    })
}
fn walk_record(
    record: &mut Record,
    visit: &mut impl FnMut(&mut Span) -> VisitResult,
) -> VisitResult {
    visit(&mut record.span)?;
    walk_annotations(&mut record.annotations, visit)
}
fn walk_trait(trait_: &mut Trait, visit: &mut impl FnMut(&mut Span) -> VisitResult) -> VisitResult {
    visit(&mut trait_.span)?;
    walk_annotations(&mut trait_.annotations, visit)?;
    for function in &mut trait_.methods {
        walk_function(function, visit)?;
    }
    Ok(())
}
fn walk_pattern(
    pattern: &mut Pattern,
    visit: &mut impl FnMut(&mut Span) -> VisitResult,
) -> VisitResult {
    crate::with_stack(|| {
        visit(&mut pattern.span)?;
        match &mut pattern.kind {
            PatternKind::Variant(_, Some(payload))
            | PatternKind::ResolvedVariant(_, Some(payload)) => walk_pattern(payload, visit)?,
            PatternKind::Tuple(patterns) | PatternKind::Or(patterns) => {
                for pattern in patterns {
                    walk_pattern(pattern, visit)?;
                }
            }
            PatternKind::Record(fields, _) => {
                for (_, pattern) in fields {
                    walk_pattern(pattern, visit)?;
                }
            }
            _ => {}
        }
        Ok(())
    })
}
fn walk_expr(
    expression: &mut Expr,
    visit: &mut impl FnMut(&mut Span) -> VisitResult,
) -> VisitResult {
    crate::with_stack(|| {
        visit(&mut expression.span)?;
        match &mut expression.kind {
            ExprKind::EnumConstruct(_, _, payload) => {
                if let Some(payload) = payload {
                    walk_expr(payload, visit)?;
                }
            }
            ExprKind::Match(value, arms) => {
                walk_expr(value, visit)?;
                for arm in arms {
                    visit(&mut arm.span)?;
                    walk_pattern(&mut arm.pattern, visit)?;
                    if let Some(guard) = &mut arm.guard {
                        walk_expr(guard, visit)?;
                    }
                    match &mut arm.body {
                        MatchBody::Expression(value) => walk_expr(value, visit)?,
                        MatchBody::Block(body, tail) => {
                            walk_block(body, visit)?;
                            if let Some(tail) = tail {
                                walk_expr(tail, visit)?;
                            }
                        }
                    }
                }
            }
            ExprKind::Invalid(diagnostic) => walk_diagnostic(diagnostic, visit)?,
            ExprKind::ManagedNew(_, _, allocator, value) => {
                if let Some(allocator) = allocator {
                    walk_expr(allocator, visit)?;
                }
                walk_expr(value, visit)?;
            }
            ExprKind::IntrinsicCall(_, _, values)
            | ExprKind::ArrayLiteral(values)
            | ExprKind::Tuple(values)
            | ExprKind::TypedCollection(_, _, values)
            | ExprKind::GenericCall(_, _, values)
            | ExprKind::Call(_, values)
            | ExprKind::ClosureValue(_, values, _) => {
                for value in values {
                    walk_expr(value, visit)?;
                }
            }
            ExprKind::AnonymousRecord(fields)
            | ExprKind::VariantPayload(fields)
            | ExprKind::Record(_, fields)
            | ExprKind::GenericRecord(_, _, fields)
            | ExprKind::ObjectLiteral(_, fields) => {
                for (_, value) in fields {
                    walk_expr(value, visit)?;
                }
            }
            ExprKind::DeleteProperty(value)
            | ExprKind::Yield(value, _)
            | ExprKind::Field(value, _)
            | ExprKind::TupleField(value, _)
            | ExprKind::Cast(value, _)
            | ExprKind::ExtensionField(value, _, _)
            | ExprKind::Parenthesized(value)
            | ExprKind::Unary(_, value) => walk_expr(value, visit)?,
            ExprKind::Invoke(target, arguments) | ExprKind::Index(target, arguments) => {
                walk_expr(target, visit)?;
                for argument in arguments {
                    walk_expr(argument, visit)?;
                }
            }
            ExprKind::Closure(function, _) => walk_function(function, visit)?,
            ExprKind::Binary(_, a, b) | ExprKind::NativeCoerce(a, b) => {
                walk_expr(a, visit)?;
                walk_expr(b, visit)?;
            }
            ExprKind::TypeDescriptor(_)
            | ExprKind::DeferredCallable(_)
            | ExprKind::Frozen(..)
            | ExprKind::Literal(_)
            | ExprKind::Name(_) => {}
        }
        Ok(())
    })
}
fn walk_block(body: &mut [Stmt], visit: &mut impl FnMut(&mut Span) -> VisitResult) -> VisitResult {
    crate::with_stack(|| {
        for statement in body {
            visit(&mut statement.span)?;
            match &mut statement.kind {
                StmtKind::Binding(_, _, _, _, value)
                | StmtKind::Constant(_, value)
                | StmtKind::Evaluate(value)
                | StmtKind::Return(value)
                | StmtKind::Input(value) => walk_expr(value, visit)?,
                StmtKind::Compound(a, _, b) | StmtKind::Assign(a, b) => {
                    walk_expr(a, visit)?;
                    walk_expr(b, visit)?;
                }
                StmtKind::Output(values) | StmtKind::Call(_, values) => {
                    for value in values {
                        walk_expr(value, visit)?;
                    }
                }
                StmtKind::IfLet(pattern, condition, a, b) => {
                    walk_pattern(pattern, visit)?;
                    walk_expr(condition, visit)?;
                    walk_block(a, visit)?;
                    walk_block(b, visit)?;
                }
                StmtKind::If(condition, a, b) => {
                    walk_expr(condition, visit)?;
                    walk_block(a, visit)?;
                    walk_block(b, visit)?;
                }
                StmtKind::While(condition, body) | StmtKind::Repeat(body, condition) => {
                    walk_expr(condition, visit)?;
                    walk_block(body, visit)?;
                }
                StmtKind::For(_, first, last, step, body) => {
                    walk_expr(first, visit)?;
                    walk_expr(last, visit)?;
                    if let Some(step) = step {
                        walk_expr(step, visit)?;
                    }
                    walk_block(body, visit)?;
                }
                StmtKind::ForIn(_, iterable, body) => {
                    walk_expr(iterable, visit)?;
                    walk_block(body, visit)?;
                }
                StmtKind::Case(value, arms, otherwise) => {
                    walk_expr(value, visit)?;
                    for (label, body) in arms {
                        walk_expr(&mut label.lower, visit)?;
                        if let Some(upper) = &mut label.upper {
                            walk_expr(upper, visit)?;
                        }
                        walk_block(body, visit)?;
                    }
                    walk_block(otherwise, visit)?;
                }
                StmtKind::Break
                | StmtKind::Continue
                | StmtKind::ReturnVoid
                | StmtKind::Declare(_, _) => {}
            }
        }
        Ok(())
    })
}
fn walk_ast(ast: &mut Ast, visit: &mut impl FnMut(&mut Span) -> VisitResult) -> VisitResult {
    crate::with_stack(|| {
        for declaration in &mut ast.enums {
            visit(&mut declaration.span)?;
            walk_annotations(&mut declaration.annotations, visit)?;
            for variant in &mut declaration.variants {
                visit(&mut variant.span)?;
                walk_annotations(&mut variant.annotations, visit)?;
            }
        }
        walk_block(&mut ast.body, visit)?;
        for function in &mut ast.functions {
            walk_function(function, visit)?;
        }
        for alias in &mut ast.type_aliases {
            visit(&mut alias.span)?;
        }
        for record in &mut ast.records {
            walk_record(record, visit)?;
        }
        for trait_ in &mut ast.traits {
            walk_trait(trait_, visit)?;
        }
        for implementation in &mut ast.implementations {
            visit(&mut implementation.span)?;
            for function in &mut implementation.methods {
                walk_function(function, visit)?;
            }
        }
        for implementation in &mut ast.inherent_implementations {
            visit(&mut implementation.span)?;
            for method in &mut implementation.methods {
                walk_function(&mut method.function, visit)?;
            }
        }
        for property in &mut ast.properties {
            visit(&mut property.span)?;
            walk_function(&mut property.getter, visit)?;
        }
        for import in &mut ast.imports {
            visit(&mut import.span)?;
        }
        for world in &mut ast.worlds {
            visit(&mut world.span)?;
            walk_annotations(&mut world.annotations, visit)?;
            for item in &mut world.items {
                match item {
                    WorldItem::Include { span, .. } => visit(span)?,
                    WorldItem::Import { span, members, .. } => {
                        visit(span)?;
                        for function in members.iter_mut().flatten() {
                            walk_function(function, visit)?;
                        }
                    }
                    WorldItem::Export { function } => walk_function(function, visit)?,
                }
            }
        }
        for implementation in &mut ast.interface_implementations {
            visit(&mut implementation.span)?;
        }
        for module in &mut ast.modules {
            visit(&mut module.span)?;
            walk_annotations(&mut module.annotations, visit)?;
            if let Some(body) = &mut module.body {
                walk_ast(body, visit)?;
            }
        }
        for native in &mut ast.native.enums {
            visit(&mut native.declaration.span)?;
            walk_annotations(&mut native.declaration.annotations, visit)?;
            for variant in &mut native.declaration.variants {
                visit(&mut variant.span)?;
                walk_annotations(&mut variant.annotations, visit)?;
            }
        }
        for declaration in &mut ast.native.types {
            visit(&mut declaration.annotation.span)?;
        }
        for declaration in &mut ast.native.functions {
            walk_function(&mut declaration.function, visit)?;
        }
        for declaration in &mut ast.native.methods {
            walk_function(&mut declaration.function, visit)?;
        }
        for (_, trait_) in &mut ast.native.traits {
            walk_trait(trait_, visit)?;
        }
        for (_, record) in &mut ast.native.families {
            walk_record(record, visit)?;
        }
        if let Some(policy) = &mut ast.native.import_policy {
            let mut changed = false;
            let mut positions = Vec::with_capacity(policy.requesters.len());
            for (position, declaration) in &policy.requesters {
                let mut span = Span::new(*position, *position);
                visit(&mut span)?;
                changed |= span.start != *position;
                positions.push((span.start, *declaration));
            }
            if changed {
                policy.requesters = positions.into_iter().collect();
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn original() -> Span {
        Span::new(1, 3)
    }
    fn relocated() -> Span {
        Span::new(8, 10)
    }
    fn annotation() -> DeclarationAnnotation {
        DeclarationAnnotation {
            name: "cyallo_internal".into(),
            value: "fixture".into(),
            span: original(),
        }
    }
    fn expression() -> Expr {
        Expr {
            kind: ExprKind::Literal(Literal::Integer(1)),
            span: original(),
        }
    }
    fn statement() -> Stmt {
        Stmt {
            kind: StmtKind::Evaluate(expression()),
            span: original(),
        }
    }
    fn function() -> Function {
        Function {
            annotations: vec![annotation()],
            body_kind: FunctionBodyKind::Source,
            constraints: vec![],
            type_packs: vec![],
            generator: false,
            source_name: "f".into(),
            origin: RoutineOrigin::Source,
            attributes: RoutineAttributes {
                heat_span: Some(original()),
                compilation_span: Some(original()),
                inline_span: Some(original()),
                ..Default::default()
            },
            type_params: vec![],
            kind: RoutineKind::Function,
            name: "f".into(),
            params: vec![Param {
                arity: ParameterArity::One,
                passing: ParameterPassing::Value,
                receiver: false,
                name: "p".into(),
                ty: Type::Integer,
                by_ref: false,
                span: original(),
            }],
            result: Type::Integer,
            body: vec![statement()],
            span: original(),
        }
    }
    fn record() -> Record {
        Record {
            opaque: false,
            annotations: vec![annotation()],
            type_params: vec![],
            name: "R".into(),
            fields: vec![],
            span: original(),
        }
    }
    fn trait_() -> Trait {
        Trait {
            type_params: vec![],
            annotations: vec![annotation()],
            origin: TraitOrigin::Source,
            name: "T".into(),
            methods: vec![function()],
            span: original(),
        }
    }

    #[test]
    fn shifts_declarations_inline_modules_closures_and_native_metadata() {
        let mut ast = Ast::default();
        ast.functions.push(function());
        ast.records.push(record());
        ast.traits.push(trait_());
        ast.implementations.push(Implementation {
            trait_name: "T".into(),
            receiver: Type::Integer,
            methods: vec![function()],
            span: original(),
        });
        ast.inherent_implementations.push(InherentImplementation {
            type_params: vec![],
            receiver: Type::Integer,
            methods: vec![InherentMethod {
                public: true,
                function: function(),
            }],
            span: original(),
        });
        ast.properties.push(Property {
            receiver: Type::Integer,
            name: "p".into(),
            getter: function(),
            span: original(),
        });
        ast.imports.push(Import {
            path: "other".into(),
            alias: None,
            span: original(),
            public: false,
            glob: false,
        });
        let mut child = Ast::default();
        child.functions.push(function());
        ast.modules.push(ModuleDeclaration {
            interface: false,
            boundary: Default::default(),
            annotations: vec![annotation()],
            name: "child".into(),
            public: true,
            body: Some(Box::new(child)),
            span: original(),
        });
        let closure = Expr {
            kind: ExprKind::Closure(Box::new(function()), vec![]),
            span: original(),
        };
        ast.body.push(Stmt {
            kind: StmtKind::If(
                expression(),
                vec![Stmt {
                    kind: StmtKind::Evaluate(closure),
                    span: original(),
                }],
                vec![Stmt {
                    kind: StmtKind::Case(
                        expression(),
                        vec![(
                            CaseLabel {
                                lower: expression(),
                                upper: Some(expression()),
                            },
                            vec![statement()],
                        )],
                        vec![statement()],
                    ),
                    span: original(),
                }],
            ),
            span: original(),
        });
        ast.native.types.push(NativeTypeDeclaration {
            id: DeclarationId(1),
            name: "N".into(),
            parameters: vec![],
            annotation: annotation(),
        });
        ast.native.functions.push(NativeFunctionDeclaration {
            id: DeclarationId(2),
            module: Some(DeclarationId(9)),
            function: function(),
        });
        ast.native.methods.push(NativeMethodDeclaration {
            id: DeclarationId(3),
            module: None,
            owner: DeclarationId(1),
            owner_parameters: vec![],
            receiver: Type::Integer,
            function: function(),
            public: true,
        });
        ast.native.traits.push((DeclarationId(4), trait_()));
        ast.native.families.push((DeclarationId(5), record()));
        ast.native.import_policy = Some(NativeImportPolicy {
            requesters: [(1, DeclarationId(9))].into(),
            ..Default::default()
        });
        shift_ast(&mut ast, 7).unwrap();
        let f = &ast.functions[0];
        assert_eq!(f.span, relocated());
        assert_eq!(f.params[0].span, relocated());
        assert_eq!(f.annotations[0].span, relocated());
        assert_eq!(f.attributes.heat_span, Some(relocated()));
        assert_eq!(f.attributes.compilation_span, Some(relocated()));
        assert_eq!(f.attributes.inline_span, Some(relocated()));
        assert_eq!(f.body[0].span, relocated());
        assert_eq!(ast.records[0].annotations[0].span, relocated());
        assert_eq!(ast.records[0].span, relocated());
        assert_eq!(ast.traits[0].span, relocated());
        assert_eq!(ast.traits[0].annotations[0].span, relocated());
        assert_eq!(ast.traits[0].methods[0].span, relocated());
        assert_eq!(ast.implementations[0].span, relocated());
        assert_eq!(ast.implementations[0].methods[0].span, relocated());
        assert_eq!(ast.inherent_implementations[0].span, relocated());
        assert_eq!(
            ast.inherent_implementations[0].methods[0].function.params[0].span,
            relocated()
        );
        assert_eq!(ast.properties[0].span, relocated());
        assert_eq!(ast.properties[0].getter.span, relocated());
        assert_eq!(ast.imports[0].span, relocated());
        assert_eq!(ast.modules[0].span, relocated());
        assert_eq!(ast.modules[0].annotations[0].span, relocated());
        assert_eq!(
            ast.modules[0].body.as_ref().unwrap().functions[0].annotations[0].span,
            relocated()
        );
        let StmtKind::If(condition, yes, no) = &ast.body[0].kind else {
            panic!()
        };
        assert_eq!(condition.span, relocated());
        let StmtKind::Evaluate(closure) = &yes[0].kind else {
            panic!()
        };
        let ExprKind::Closure(closure, _) = &closure.kind else {
            panic!()
        };
        assert_eq!(closure.span, relocated());
        assert_eq!(closure.params[0].span, relocated());
        let StmtKind::Case(_, arms, otherwise) = &no[0].kind else {
            panic!()
        };
        assert_eq!(arms[0].0.lower.span, relocated());
        assert_eq!(arms[0].0.upper.as_ref().unwrap().span, relocated());
        assert_eq!(arms[0].1[0].span, relocated());
        assert_eq!(otherwise[0].span, relocated());
        assert_eq!(ast.native.types[0].annotation.span, relocated());
        assert_eq!(
            ast.native.functions[0].function.annotations[0].span,
            relocated()
        );
        assert_eq!(ast.native.methods[0].function.span, relocated());
        assert_eq!(ast.native.functions[0].module, Some(DeclarationId(9)));
        assert_eq!(ast.native.traits[0].1.span, relocated());
        assert_eq!(ast.native.families[0].1.annotations[0].span, relocated());
        assert_eq!(
            ast.native
                .import_policy
                .as_ref()
                .unwrap()
                .requesters
                .get(&8),
            Some(&DeclarationId(9))
        );
    }

    #[test]
    fn invalid_expression_diagnostics_and_related_spans_are_shifted() {
        let diagnostic =
            Diagnostic::new("fixture", "failure", original()).with_related(original(), "related");
        let mut ast = Ast {
            body: vec![Stmt {
                kind: StmtKind::Evaluate(Expr {
                    kind: ExprKind::Invalid(diagnostic),
                    span: original(),
                }),
                span: original(),
            }],
            ..Default::default()
        };
        shift_ast(&mut ast, 7).unwrap();
        let StmtKind::Evaluate(expression) = &ast.body[0].kind else {
            panic!()
        };
        let ExprKind::Invalid(diagnostic) = &expression.kind else {
            panic!()
        };
        assert_eq!(expression.span, relocated());
        assert_eq!(diagnostic.span, relocated());
        assert_eq!(diagnostic.related[0].0, relocated());
    }

    #[test]
    fn overflow_is_typed_and_does_not_partially_mutate_the_ast() {
        let mut ast = Ast {
            body: vec![statement()],
            records: vec![Record {
                span: Span::new(u32::MAX - 1, u32::MAX),
                ..record()
            }],
            ..Default::default()
        };
        let error = shift_ast_checked(&mut ast, 1).unwrap_err();
        assert!(matches!(
            error,
            SpanShiftError::Overflow {
                endpoint: SpanEndpoint::End,
                offset: 1,
                ..
            }
        ));
        assert_eq!(ast.body[0].span, original());
        let StmtKind::Evaluate(expression) = &ast.body[0].kind else {
            panic!()
        };
        assert_eq!(expression.span, original());
        assert_eq!(ast.records[0].span, Span::new(u32::MAX - 1, u32::MAX));
        assert_eq!(invalid_span_shift(error).code, "E175");
        let mut malformed = Span::new(0, 0);
        malformed.start = 3;
        malformed.end = 1;
        ast.records[0].span = malformed;
        assert!(matches!(
            shift_ast_checked(&mut ast, 0),
            Err(SpanShiftError::InvalidRange { .. })
        ));
    }

    #[test]
    fn diagnostic_relocation_is_also_transactional() {
        let mut diagnostic = Diagnostic::new("fixture", "failure", original())
            .with_related(Span::new(u32::MAX, u32::MAX), "related");
        assert!(shift_diagnostic(&mut diagnostic, 1).is_err());
        assert_eq!(diagnostic.span, original());
        assert_eq!(diagnostic.related[0].0, Span::new(u32::MAX, u32::MAX));
    }
}
