//! Compile-time adapters from declaration-owned native values to ordinary
//! capture-free closures. No runtime operation is selected from a source name.

use crate::{CompileOptions, analysis, declarations::TypeEnvironment, inference, native_contract};
use cyallo_ast::*;
use std::collections::HashMap;

type Result<T> = std::result::Result<T, Diagnostic>;
type Known = HashMap<String, Type>;

/// Runs after declaration/generic resolution and initial inference, before
/// closure lifting. Each invocation owns its traversal and generated names.
pub fn rewrite(ast: &mut Ast, env: &TypeEnvironment, options: CompileOptions) -> Result<()> {
    let mut globals = analysis::collect(&ast.body, &[])?;
    inference::infer(&ast.body, &mut globals, &[], env, options)?;
    rewrite_with_globals(ast, env, options, &globals)
}
pub fn rewrite_with_globals(
    ast: &mut Ast,
    env: &TypeEnvironment,
    options: CompileOptions,
    globals: &[crate::ir::Local],
) -> Result<()> {
    let known: Known = globals
        .iter()
        .map(|l| (l.name.to_string(), l.ty.clone()))
        .collect();
    let mut rewriter = Rewriter { env, options };
    rewriter.block(&mut ast.body, &known, None, None)?;
    for function in &mut ast.functions {
        if function.body_kind == FunctionBodyKind::Source && function.type_params.is_empty() {
            rewriter.function(function, &known)?;
        }
    }
    Ok(())
}

struct Rewriter<'a> {
    env: &'a TypeEnvironment,
    options: CompileOptions,
}

impl Rewriter<'_> {
    fn ty(&self, expression: &Expr, known: &Known) -> Result<Type> {
        inference::expression_type(expression, known, self.env)
    }

    fn needs_adapters(&self, function: &mut Function, outer: &Known) -> Result<bool> {
        fn contains_native(ty: &Type) -> bool {
            let mut found = false;
            cyallo_ast::visit::visit_type(&mut ty.clone(), &mut |ty| {
                found |= matches!(ty, Type::NativeCallable(_))
            });
            found
        }
        let mut needed = function
            .params
            .iter()
            .any(|param| contains_native(&param.ty))
            || contains_native(&function.result);
        cyallo_ast::visit::visit_block(
            &mut function.body,
            &mut |expression| {
                match &mut expression.kind {
                    ExprKind::Name(name) => {
                        needed |= outer
                            .get(name.as_ref())
                            .or_else(|| self.env.get(name.as_ref()))
                            .is_some_and(contains_native);
                    }
                    ExprKind::Closure(nested, _) => needed |= self.needs_adapters(nested, outer)?,
                    _ => {}
                }
                Ok(())
            },
            &mut |_, _| Ok(()),
        )?;
        Ok(needed)
    }
    fn function(&mut self, function: &mut Function, outer: &Known) -> Result<()> {
        // Ordinary bodies need no native-value conversion. Leave their local
        // inference to admission, especially while a const evaluator has only
        // initialized a prefix of the module's global declarations.
        if !self.needs_adapters(function, outer)? {
            return Ok(());
        }
        let globals: Vec<_> = outer
            .iter()
            .map(|(name, ty)| crate::ir::Local {
                name: name.clone().into(),
                display_name: None,
                ty: ty.clone(),
                constant: false,
                span: function.span,
            })
            .collect();
        let mut locals = analysis::collect(&function.body, &function.params)?;
        crate::generator::input_local(function, &mut locals);
        inference::infer(
            &function.body,
            &mut locals,
            &globals,
            self.env,
            self.options,
        )?;
        let mut known = outer.clone();
        known.extend(
            locals
                .into_iter()
                .map(|local| (local.name.to_string(), local.ty)),
        );
        let result = crate::generator::result(function);
        let yielded = if function.generator {
            crate::generator::parts(&function.result).map(|(yielded, _, _)| yielded.clone())
        } else {
            None
        };
        self.block(&mut function.body, &known, Some(&result), yielded.as_ref())
    }

    fn block(
        &mut self,
        body: &mut [Stmt],
        known: &Known,
        returned: Option<&Type>,
        yielded: Option<&Type>,
    ) -> Result<()> {
        cyallo_ast::with_stack(|| {
            for statement in body {
                match &mut statement.kind {
                    StmtKind::IfLet(..) => {
                        return Err(crate::patterns::invalid_pattern(
                            crate::patterns::PatternError::UnresolvedPattern,
                            statement.span,
                        ));
                    }
                    StmtKind::Binding(name, annotation, _, _, value) => {
                        let expected = annotation.as_ref().or_else(|| known.get(name.as_ref()));
                        self.expression(value, expected, known, yielded)?;
                    }
                    StmtKind::Return(value) => self.expression(value, returned, known, yielded)?,
                    StmtKind::Assign(target, value) => {
                        self.expression(target, None, known, yielded)?;
                        let expected = self.ty(target, known).ok();
                        self.expression(value, expected.as_ref(), known, yielded)?;
                    }
                    StmtKind::Compound(target, _, value) => {
                        self.expression(target, None, known, yielded)?;
                        self.expression(value, None, known, yielded)?;
                    }
                    StmtKind::Constant(_, value)
                    | StmtKind::Evaluate(value)
                    | StmtKind::Input(value) => self.expression(value, None, known, yielded)?,
                    StmtKind::Output(values) => self.arguments(values, &[], known, yielded)?,
                    StmtKind::Call(name, values) => {
                        let parameters = self.named_parameters(name, values, known)?;
                        self.arguments(values, &parameters, known, yielded)?;
                    }
                    StmtKind::If(condition, yes, no) => {
                        self.expression(condition, Some(&Type::Boolean), known, yielded)?;
                        self.block(yes, known, returned, yielded)?;
                        self.block(no, known, returned, yielded)?;
                    }
                    StmtKind::While(condition, body) | StmtKind::Repeat(body, condition) => {
                        self.expression(condition, Some(&Type::Boolean), known, yielded)?;
                        self.block(body, known, returned, yielded)?;
                    }
                    StmtKind::For(_, first, last, step, body) => {
                        self.expression(first, None, known, yielded)?;
                        self.expression(last, None, known, yielded)?;
                        if let Some(step) = step {
                            self.expression(step, None, known, yielded)?;
                        }
                        self.block(body, known, returned, yielded)?;
                    }
                    StmtKind::ForIn(_, iterable, body) => {
                        self.expression(iterable, None, known, yielded)?;
                        self.block(body, known, returned, yielded)?;
                    }
                    StmtKind::Case(value, arms, otherwise) => {
                        self.expression(value, None, known, yielded)?;
                        for (label, body) in arms {
                            self.expression(&mut label.lower, None, known, yielded)?;
                            if let Some(upper) = &mut label.upper {
                                self.expression(upper, None, known, yielded)?;
                            }
                            self.block(body, known, returned, yielded)?;
                        }
                        self.block(otherwise, known, returned, yielded)?;
                    }
                    StmtKind::Break
                    | StmtKind::Continue
                    | StmtKind::ReturnVoid
                    | StmtKind::Declare(..) => {}
                }
            }
            Ok(())
        })
    }

    fn argument_types(
        &self,
        arguments: &[Expr],
        known: &Known,
        parameters: &[Type],
    ) -> Result<Vec<Type>> {
        arguments
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                let contextual = parameters
                    .get(index)
                    .map(|ty| analysis::contextualize(argument, ty))
                    .transpose()?;
                self.ty(contextual.as_ref().unwrap_or(argument), known)
            })
            .collect()
    }

    fn native_parameters(
        &self,
        declaration: DeclarationId,
        type_args: &[Type],
        arguments: &[Expr],
        known: &Known,
    ) -> Result<Vec<Type>> {
        if !self.has_native_arguments(arguments, known) {
            return Ok(vec![]);
        }
        // Initial fixed types contextualize literal operands; a second pass also
        // specializes callback contracts from the other typed operands.
        let actual = self.argument_types(arguments, known, &[])?;
        let parameters = native_contract::function_parameter_types(
            self.env,
            declaration,
            type_args,
            arguments,
            &actual,
        )?;
        let actual = self.argument_types(arguments, known, &parameters)?;
        native_contract::function_parameter_types(
            self.env,
            declaration,
            type_args,
            arguments,
            &actual,
        )
    }

    fn has_native_arguments(&self, arguments: &[Expr], known: &Known) -> bool {
        arguments
            .iter()
            .filter_map(|argument| self.ty(argument, known).ok())
            .any(|mut ty| {
                let mut found = false;
                cyallo_ast::visit::visit_type(&mut ty, &mut |ty| {
                    found |= matches!(ty, Type::NativeCallable(_))
                });
                found
            })
    }

    fn named_parameters(&self, name: &str, arguments: &[Expr], known: &Known) -> Result<Vec<Type>> {
        match known.get(name).or_else(|| self.env.get(name)) {
            Some(Type::NativeCallable(declaration)) => {
                self.native_parameters(*declaration, &[], arguments, known)
            }
            Some(ty) => Ok(crate::composites::callable(ty)
                .map(|(_, parameters, _)| parameters.iter().map(|(ty, _)| ty.clone()).collect())
                .unwrap_or_default()),
            None => Ok(vec![]),
        }
    }

    fn ordinary_method_parameters(&self, receiver: &Type, name: &str) -> Option<Vec<Type>> {
        if matches!(receiver.pointee_or_self(), Type::Record(_, fields) | Type::AnonymousRecord(fields) if fields.iter().any(|(field, _)| field.as_ref() == name))
        {
            return None;
        }
        let concrete = format!("$CONCRETE${receiver:?}.{name}");
        let trait_type = match receiver.pointee_or_self() {
            Type::ServiceRef(inner) => inner.as_ref(),
            ty => ty,
        };
        let trait_method = match trait_type {
            Type::TraitObject(owner) => Some(format!("$METHOD${owner}.{name}")),
            _ => None,
        };
        self.env
            .get(&concrete)
            .or_else(|| {
                trait_method
                    .as_ref()
                    .and_then(|method| self.env.get(method))
            })
            .and_then(crate::composites::callable)
            .map(|(_, parameters, _)| parameters.iter().map(|(ty, _)| ty.clone()).collect())
    }

    fn arguments(
        &mut self,
        arguments: &mut [Expr],
        parameters: &[Type],
        known: &Known,
        yielded: Option<&Type>,
    ) -> Result<()> {
        for (index, argument) in arguments.iter_mut().enumerate() {
            self.expression(argument, parameters.get(index), known, yielded)?;
        }
        Ok(())
    }

    fn expression(
        &mut self,
        expression: &mut Expr,
        expected: Option<&Type>,
        known: &Known,
        yielded: Option<&Type>,
    ) -> Result<()> {
        cyallo_ast::with_stack(|| {
            match &mut expression.kind {
                ExprKind::EnumConstruct(_, ty, payload) => {
                    if let Some(payload) = payload {
                        let expected = None;
                        let _ = ty;
                        self.expression(payload, expected, known, yielded)?;
                    }
                }
                ExprKind::AnonymousRecord(fields) | ExprKind::VariantPayload(fields) => {
                    let declared = match expected {
                        Some(Type::AnonymousRecord(fields)) => Some(fields),
                        _ => None,
                    };
                    for (name, value) in fields {
                        self.expression(
                            value,
                            declared.and_then(|fields| {
                                fields
                                    .iter()
                                    .find(|(field, _)| field == name)
                                    .map(|(_, ty)| ty)
                            }),
                            known,
                            yielded,
                        )?;
                    }
                }
                ExprKind::Match(value, arms) => {
                    self.expression(value, None, known, yielded)?;
                    let ty = crate::patterns::scrutinee_type(value, known, self.env)?;
                    for arm in arms {
                        let mut scope = known.clone();
                        for binding in
                            crate::patterns::check(&arm.pattern, &ty, self.env)?.bindings()
                        {
                            scope.insert(binding.name.to_string(), binding.ty);
                        }
                        if let Some(guard) = &mut arm.guard {
                            self.expression(guard, Some(&Type::Boolean), &scope, yielded)?;
                        }
                        match &mut arm.body {
                            MatchBody::Expression(value) => {
                                self.expression(value, expected, &scope, yielded)?
                            }
                            MatchBody::Block(body, tail) => {
                                self.block(body, &scope, None, yielded)?;
                                if let Some(tail) = tail {
                                    self.expression(tail, expected, &scope, yielded)?;
                                }
                            }
                        }
                    }
                }
                // Already checked adapters must not acquire another adapter.
                ExprKind::NativeCoerce(..) => return Ok(()),
                ExprKind::Closure(function, _) => self.function(function, known)?,
                ExprKind::IntrinsicCall(id, type_args, arguments) => {
                    let parameters = if let Some(declaration) = id.1 {
                        self.native_parameters(declaration, type_args, arguments, known)?
                    } else {
                        vec![]
                    };
                    self.arguments(arguments, &parameters, known, yielded)?;
                }
                ExprKind::Call(name, arguments) => {
                    let parameters = self.named_parameters(name, arguments, known)?;
                    self.arguments(arguments, &parameters, known, yielded)?;
                }
                ExprKind::Invoke(callee, arguments) => {
                    let mut parameters = None;
                    let native_arguments = self.has_native_arguments(arguments, known);
                    if native_arguments
                        && let ExprKind::Field(receiver, name)
                        | ExprKind::ExtensionField(receiver, name, _) = &mut callee.kind
                    {
                        self.expression(receiver, None, known, yielded)?;
                        let receiver_type = self.ty(receiver, known)?;
                        parameters = self.ordinary_method_parameters(&receiver_type, name);
                        if parameters.is_none() {
                            let initial = native_contract::parameter_types(
                                self.env,
                                &receiver_type,
                                name,
                                expression.span,
                            )
                            .unwrap_or_default();
                            let actual = self.argument_types(arguments, known, &initial)?;
                            if let Some(method) = native_contract::member(
                                self.env,
                                &receiver_type,
                                name,
                                arguments,
                                &actual,
                                expression.span,
                            )? {
                                parameters = Some(
                                    method
                                        .callable
                                        .params
                                        .into_iter()
                                        .map(|parameter| parameter.ty)
                                        .collect::<Vec<_>>(),
                                );
                            }
                        }
                    }
                    if parameters.is_none() {
                        self.expression(callee, None, known, yielded)?;
                        parameters = Some(match self.ty(callee, known).ok() {
                            Some(Type::NativeCallable(declaration)) => {
                                self.native_parameters(declaration, &[], arguments, known)?
                            }
                            Some(ty) => crate::composites::callable(&ty)
                                .map(|(_, parameters, _)| {
                                    parameters.iter().map(|(ty, _)| ty.clone()).collect()
                                })
                                .unwrap_or_default(),
                            None => vec![],
                        });
                    }
                    self.arguments(arguments, &parameters.unwrap(), known, yielded)?;
                }
                ExprKind::Parenthesized(inner) => {
                    self.expression(inner, expected, known, yielded)?
                }
                ExprKind::Cast(inner, ty) => self.expression(inner, Some(ty), known, yielded)?,
                ExprKind::ArrayLiteral(values) => {
                    let element = match expected {
                        Some(
                            Type::Array(_, element)
                            | Type::SparseArray(element)
                            | Type::Slice(element),
                        ) => Some(element.as_ref()),
                        _ => None,
                    };
                    for value in values {
                        self.expression(value, element, known, yielded)?;
                    }
                }
                ExprKind::Tuple(values) => {
                    let parameters = match expected {
                        Some(Type::Tuple(types)) => types.as_slice(),
                        _ => &[],
                    };
                    self.arguments(values, parameters, known, yielded)?;
                }
                ExprKind::Record(name, values) | ExprKind::GenericRecord(name, _, values) => {
                    let record = expected.or_else(|| self.env.get(name.as_ref()));
                    for (name, value) in values {
                        let ty = match record {
                            Some(Type::Record(_, fields) | Type::AnonymousRecord(fields)) => fields
                                .iter()
                                .find(|(field, _)| field == name)
                                .map(|(_, ty)| ty),
                            _ => None,
                        };
                        self.expression(value, ty, known, yielded)?;
                    }
                }
                ExprKind::ManagedNew(_, _, allocator, value) => {
                    if let Some(allocator) = allocator {
                        self.expression(allocator, None, known, yielded)?;
                    }
                    let inner = match expected {
                        Some(Type::Managed { pointee, .. }) => Some(pointee.as_ref()),
                        _ => None,
                    };
                    self.expression(value, inner, known, yielded)?;
                }
                ExprKind::Yield(value, delegate) => self.expression(
                    value,
                    if *delegate { None } else { yielded },
                    known,
                    yielded,
                )?,
                ExprKind::ObjectLiteral(_, fields) => {
                    for (_, value) in fields {
                        self.expression(value, None, known, yielded)?;
                    }
                }
                ExprKind::DeleteProperty(inner)
                | ExprKind::Unary(_, inner)
                | ExprKind::Field(inner, _)
                | ExprKind::ExtensionField(inner, _, _)
                | ExprKind::TupleField(inner, _) => self.expression(inner, None, known, yielded)?,
                ExprKind::Binary(_, left, right) => {
                    self.expression(left, None, known, yielded)?;
                    self.expression(right, None, known, yielded)?;
                }
                ExprKind::Index(base, indices) => {
                    self.expression(base, None, known, yielded)?;
                    self.arguments(indices, &[], known, yielded)?;
                }
                ExprKind::ClosureValue(_, values, _)
                | ExprKind::TypedCollection(_, _, values)
                | ExprKind::GenericCall(_, _, values) => {
                    self.arguments(values, &[], known, yielded)?
                }
                ExprKind::TypeDescriptor(_)
                | ExprKind::DeferredCallable(_)
                | ExprKind::Invalid(_)
                | ExprKind::Frozen(..)
                | ExprKind::Literal(_)
                | ExprKind::Name(_) => {}
            }
            if let Some(expected @ Type::Function(..)) = expected
                && let Ok(Type::NativeCallable(declaration)) = self.ty(expression, known)
            {
                self.env.check_native_access(declaration, expression.span)?;
                let coercion =
                    native_contract::as_callable(self.env, declaration, expected, expression.span)?;
                let thunk =
                    self.thunk(declaration, expected, coercion.type_args, expression.span)?;
                let original = expression.clone();
                expression.kind = ExprKind::NativeCoerce(Box::new(original), Box::new(thunk));
            }
            Ok(())
        })
    }

    fn thunk(
        &self,
        declaration: DeclarationId,
        expected: &Type,
        type_args: Vec<Type>,
        span: Span,
    ) -> Result<Expr> {
        let Type::Function(kind, parameters, result) = expected else {
            unreachable!("checked callable context")
        };
        let params: Vec<_> = parameters
            .iter()
            .enumerate()
            .map(|(index, (ty, by_ref))| Param {
                arity: ParameterArity::One,
                passing: ParameterPassing::Value,
                receiver: false,
                name: format!("$native_arg{index}").into(),
                ty: ty.clone(),
                by_ref: *by_ref,
                span,
            })
            .collect();
        let arguments = params
            .iter()
            .map(|parameter| Expr {
                kind: ExprKind::Name(parameter.name.clone()),
                span,
            })
            .collect();
        let call = Expr {
            kind: ExprKind::IntrinsicCall(
                self.env.function_reference(declaration, span)?,
                type_args,
                arguments,
            ),
            span,
        };
        let body = if matches!(result.as_ref(), Type::Void) {
            vec![
                Stmt {
                    kind: StmtKind::Evaluate(call),
                    span,
                },
                Stmt {
                    kind: StmtKind::ReturnVoid,
                    span,
                },
            ]
        } else if matches!(result.as_ref(), Type::Tuple(types) if types.is_empty()) {
            vec![
                Stmt {
                    kind: StmtKind::Evaluate(call),
                    span,
                },
                Stmt {
                    kind: StmtKind::Return(Expr {
                        kind: ExprKind::Tuple(vec![]),
                        span,
                    }),
                    span,
                },
            ]
        } else {
            vec![Stmt {
                kind: StmtKind::Return(call),
                span,
            }]
        };
        Ok(Expr {
            kind: ExprKind::Closure(
                Box::new(Function {
                    annotations: vec![],
                    body_kind: FunctionBodyKind::Source,
                    constraints: vec![],
                    type_packs: vec![],
                    generator: false,
                    source_name: "$native_thunk".into(),
                    origin: RoutineOrigin::Closure,
                    attributes: RoutineAttributes::default(),
                    type_params: vec![],
                    kind: *kind,
                    name: "$native_thunk".into(),
                    params,
                    result: *result.clone(),
                    body,
                    span,
                }),
                vec![],
            ),
            span,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(source: &str) -> (Ast, TypeEnvironment) {
        let mut graph = crate::namespace::single(cyallo_parser::parse(source).unwrap()).unwrap();
        let mut ast = std::mem::take(&mut graph.entry_mut().source);
        ast.native = graph.native;
        let mut types = crate::composites::resolve(&mut ast).unwrap();
        let services = crate::services::prepare(&mut ast).unwrap();
        for interface in &services.traits {
            for method in &interface.methods {
                types.insert(
                    format!("$METHOD${}.{}", interface.name, method.name),
                    Type::Function(
                        method.kind,
                        method.params.clone(),
                        Box::new(method.result.clone()),
                    ),
                );
            }
        }
        for implementation in &services.implementations {
            for method in &services.traits[implementation.trait_id].methods {
                types.insert(
                    format!("$CONCRETE${:?}.{}", implementation.receiver, method.name),
                    Type::Function(
                        method.kind,
                        method.params.clone(),
                        Box::new(method.result.clone()),
                    ),
                );
            }
        }
        types.extend(ast.functions.iter().map(|function| {
            (
                function.name.to_string(),
                crate::composites::function_type(function),
            )
        }));
        let env = TypeEnvironment::new(types, ast.native.clone());
        (ast, env)
    }

    fn adapter(expression: &Expr) -> (&Expr, &Function) {
        let ExprKind::NativeCoerce(original, thunk) = &expression.kind else {
            panic!("expected native adapter: {expression:?}")
        };
        let ExprKind::Closure(function, captures) = &thunk.kind else {
            panic!("expected ordinary closure")
        };
        assert!(captures.is_empty());
        assert!(function.annotations.is_empty());
        assert_eq!(function.body_kind, FunctionBodyKind::Source);
        (original, function)
    }

    #[test]
    fn annotated_binding_uses_ordinary_capture_free_closure_and_declared_intrinsic() {
        let (mut ast, env) = program("let callback: fn(i64) -> i64 = std::num::abs\n");
        rewrite(&mut ast, &env, CompileOptions::default()).unwrap();
        let StmtKind::Binding(_, _, _, _, value) = &ast.body[0].kind else {
            panic!()
        };
        let (original, function) = adapter(value);
        assert!(matches!(original.kind, ExprKind::Name(_)));
        assert_eq!(function.params[0].name.as_ref(), "$native_arg0");
        assert_eq!(function.params[0].passing, ParameterPassing::Value);
        assert_eq!(function.result, Type::Integer);
        assert_eq!(function.span, original.span);
        let StmtKind::Return(Expr {
            kind: ExprKind::IntrinsicCall(reference, types, arguments),
            ..
        }) = &function.body[0].kind
        else {
            panic!()
        };
        assert!(reference.1.is_some());
        assert_eq!(types, &[Type::Integer]);
        assert!(
            matches!(&arguments[0].kind, ExprKind::Name(name) if name.as_ref() == "$native_arg0")
        );
    }

    #[test]
    fn ordinary_call_parameters_and_function_returns_provide_context() {
        let (mut ast, env) = program(
            "fn producer() -> fn(f64) -> f64 { return std::num::abs }\nfn apply(callback: fn(i64) -> i64, value: i64) -> i64 { return callback(value) }\nlet answer = apply(std::num::abs, 1)\n",
        );
        rewrite(&mut ast, &env, CompileOptions::default()).unwrap();
        let StmtKind::Binding(
            _,
            _,
            _,
            _,
            Expr {
                kind: ExprKind::Call(_, arguments),
                ..
            },
        ) = &ast.body[0].kind
        else {
            panic!()
        };
        assert_eq!(adapter(&arguments[0]).1.result, Type::Integer);
        let producer = ast
            .functions
            .iter()
            .find(|function| function.source_name.as_ref() == "producer")
            .unwrap();
        let StmtKind::Return(value) = &producer.body[0].kind else {
            panic!()
        };
        assert_eq!(adapter(value).1.result, Type::Real);
    }

    #[test]
    fn ordinary_trait_method_parameters_provide_callback_context() {
        let (mut ast, env) = program(
            "trait Apply { fn apply(self: &Self, callback: fn(i64) -> i64) -> i64 }\nrecord Host {}\nimpl Apply for Host { fn apply(self: &Self, callback: fn(i64) -> i64) -> i64 { return callback(-2) } }\nlet host = Host {}\nlet answer = host.apply(std::num::abs)\n",
        );
        rewrite(&mut ast, &env, CompileOptions::default()).unwrap();
        let StmtKind::Binding(
            _,
            _,
            _,
            _,
            Expr {
                kind: ExprKind::Invoke(_, arguments),
                ..
            },
        ) = &ast.body[1].kind
        else {
            panic!()
        };
        assert_eq!(adapter(&arguments[0]).1.result, Type::Integer);
    }

    #[test]
    fn native_function_callback_contracts_are_instantiated_from_sources() {
        let (mut ast, env) = program("let task = std::task::spawn(std::io::print)\n");
        rewrite(&mut ast, &env, CompileOptions::default()).unwrap();
        let StmtKind::Binding(
            _,
            _,
            _,
            _,
            Expr {
                kind: ExprKind::IntrinsicCall(_, _, arguments),
                ..
            },
        ) = &ast.body[0].kind
        else {
            panic!()
        };
        assert_eq!(adapter(&arguments[0]).1.kind, RoutineKind::Procedure);
    }

    #[test]
    fn native_member_callback_contracts_are_instantiated_from_sources() {
        let (mut ast, env) = program(
            "let context = std::service::Context::new()\ncontext.on_dispose(std::io::print)\n",
        );
        rewrite(&mut ast, &env, CompileOptions::default()).unwrap();
        let StmtKind::Evaluate(Expr {
            kind: ExprKind::Invoke(_, arguments),
            ..
        }) = &ast.body[1].kind
        else {
            panic!()
        };
        assert_eq!(adapter(&arguments[0]).1.kind, RoutineKind::Procedure);
    }

    #[test]
    fn adapter_retains_side_effectful_descriptor_expression_and_uninitialized_read() {
        let (mut ast, mut env) = program("let callback: fn(i64) -> i64 = std::num::abs\n");
        let declaration = env
            .native
            .functions
            .iter()
            .find(|entry| entry.function.annotations[0].value.as_ref() == "numeric_abs")
            .unwrap()
            .id;
        env.insert(
            "descriptor_factory".into(),
            Type::Function(
                RoutineKind::Procedure,
                vec![],
                Box::new(Type::NativeCallable(declaration)),
            ),
        );
        let StmtKind::Binding(_, _, _, _, value) = &mut ast.body[0].kind else {
            panic!()
        };
        value.kind = ExprKind::Call("descriptor_factory".into(), vec![]);
        rewrite(&mut ast, &env, CompileOptions::default()).unwrap();
        let StmtKind::Binding(_, _, _, _, value) = &ast.body[0].kind else {
            panic!()
        };
        assert!(matches!(adapter(value).0.kind, ExprKind::Call(..)));

        let (mut ast, mut env) = program("let callback: fn(i64) -> i64 = std::num::abs\n");
        env.insert("uninitialized".into(), Type::NativeCallable(declaration));
        let StmtKind::Binding(_, _, _, _, value) = &mut ast.body[0].kind else {
            panic!()
        };
        value.kind = ExprKind::Name("uninitialized".into());
        rewrite(&mut ast, &env, CompileOptions::default()).unwrap();
        let StmtKind::Binding(_, _, _, _, value) = &ast.body[0].kind else {
            panic!()
        };
        assert!(
            matches!(&adapter(value).0.kind, ExprKind::Name(name) if name.as_ref() == "uninitialized")
        );
    }

    #[test]
    fn unit_and_void_adapters_evaluate_call_before_matching_return() {
        let (ast, env) = program("");
        let declaration = ast
            .native
            .functions
            .iter()
            .find(|entry| entry.function.annotations[0].value.as_ref() == "print")
            .unwrap()
            .id;
        let rewriter = Rewriter {
            env: &env,
            options: CompileOptions::default(),
        };
        for result in [Type::Void, Type::Tuple(vec![])] {
            let expected = Type::Function(RoutineKind::Procedure, vec![], Box::new(result.clone()));
            let closure = rewriter
                .thunk(declaration, &expected, vec![], Span::new(1, 2))
                .unwrap();
            let ExprKind::Closure(function, _) = closure.kind else {
                panic!()
            };
            assert!(matches!(function.body[0].kind, StmtKind::Evaluate(_)));
            match result {
                Type::Void => assert!(matches!(function.body[1].kind, StmtKind::ReturnVoid)),
                _ => assert!(
                    matches!(&function.body[1].kind, StmtKind::Return(Expr {kind: ExprKind::Tuple(values), ..}) if values.is_empty())
                ),
            }
        }
    }

    #[test]
    fn pass_leaves_uncontextualized_native_aliases_and_unrelated_bodies_for_normal_checking() {
        let (mut ast, env) =
            program("fn unused() -> i64 { return missing }\nlet alias = std::num::abs\n");
        rewrite(&mut ast, &env, CompileOptions::default()).unwrap();
        let StmtKind::Binding(_, _, _, _, value) = &ast.body[0].kind else {
            panic!()
        };
        assert!(matches!(value.kind, ExprKind::Name(_)));
    }
}
