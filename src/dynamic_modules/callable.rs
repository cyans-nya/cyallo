//! Instantiate dynamic call descriptors with the ordinary source compiler.
use super::*;
use cyallo_ir::{declarations::TypeEnvironment, dynamic::CallableTarget};
use cyallo_shapes::DynamicError as E;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallRequest {
    pub target: CallableTarget,
    pub arguments: Vec<Type>,
    pub expected: Option<Type>,
    pub span: Span,
}
impl CallRequest {
    pub fn same_contract(&self, other: &Self) -> bool {
        self.target == other.target
            && self.arguments == other.arguments
            && self.expected == other.expected
            && self.span == other.span
    }
}
#[derive(Debug, Clone)]
pub(crate) struct Adapter {
    pub request: CallRequest,
    pub function: usize,
}
pub(crate) struct CompiledCalls {
    pub program: Arc<ir::Program>,
    pub adapters: Vec<Adapter>,
    pub initializers: Vec<usize>,
}
fn compile_error(error: LoadError) -> E {
    E::Call(Box::new(error))
}

pub(crate) fn specialize_calls(
    mut program: Arc<ir::Program>,
    requests: Vec<CallRequest>,
) -> Result<CompiledCalls, E> {
    let mut adapters = vec![];
    let mut initializers = vec![];
    for request in requests {
        let mut plan = program
            .dynamic_plan
            .as_ref()
            .ok_or_else(|| compile_error(LoadError::Publication(P::MissingCompilePlan)))?
            .as_ref()
            .clone();
        if let Some((_, adapter)) = plan
            .calls
            .iter()
            .find(|(old, _)| old.same_contract(&request))
        {
            adapters.push(adapter.clone());
            continue;
        }
        let core = plan.core.view();
        let mut linked = crate::namespace::link_with_core(
            plan.asts.clone(),
            &plan.paths,
            plan.entry,
            &plan.policy,
            &core,
            plan.core_base,
        )
        .map_err(E::CallCheck)?;
        let span = request.span;
        let name: Box<str> = format!("$dynamicCall{}", plan.next_factory).into();
        plan.next_factory += 1;
        let params: Vec<_> = request
            .arguments
            .iter()
            .enumerate()
            .map(|(i, ty)| Param {
                arity: ast::ParameterArity::One,
                passing: ast::ParameterPassing::Value,
                receiver: false,
                name: format!("$argument{i}").into(),
                ty: ty.clone(),
                by_ref: false,
                span,
            })
            .collect();
        let args: Vec<_> = params
            .iter()
            .map(|p| expression(ExprKind::Name(p.name.clone()), span))
            .collect();
        let env = TypeEnvironment::new(HashMap::new(), linked.native.clone());
        let call = match &request.target {
            CallableTarget::Native(id) => {
                let type_args = if let Some(expected) = &request.expected {
                    cyallo_ir::native_contract::as_callable(&env, *id, expected, span)
                        .map_err(E::CallCheck)?
                        .type_args
                } else {
                    vec![]
                };
                expression(
                    ExprKind::IntrinsicCall(
                        env.function_reference(*id, span).map_err(E::CallCheck)?,
                        type_args,
                        args,
                    ),
                    span,
                )
            }
            CallableTarget::Source(target) => {
                expression(ExprKind::Call(target.clone(), args), span)
            }
        };
        linked.functions.push(function(
            name.clone(),
            params.clone(),
            Type::Dynamic,
            vec![Stmt {
                kind: StmtKind::Return(expression(
                    ExprKind::Cast(Box::new(call), Type::Dynamic),
                    span,
                )),
                span,
            }],
            span,
        ));
        cyallo_ir::generics::specialize(&mut linked, plan.options).map_err(E::CallCheck)?;
        let mut env = TypeEnvironment::new(
            cyallo_ir::composites::resolve(&mut linked).map_err(E::CallCheck)?,
            linked.native.clone(),
        );
        env.extend(
            linked
                .functions
                .iter()
                .map(|f| (f.name.to_string(), cyallo_ir::composites::function_type(f))),
        );
        let wrapper = linked
            .functions
            .iter_mut()
            .find(|f| f.name == name)
            .unwrap();
        let StmtKind::Return(erased) = &wrapper.body[0].kind else {
            unreachable!()
        };
        let ExprKind::Cast(call, _) = &erased.kind else {
            unreachable!()
        };
        let call = *call.clone();
        let known = params
            .iter()
            .map(|p| (p.name.to_string(), p.ty.clone()))
            .collect();
        let result =
            cyallo_ir::inference::expression_type(&call, &known, &env).map_err(E::CallCheck)?;
        let kind = match &request.target {
            CallableTarget::Native(id) => {
                cyallo_ir::native_contract::function(
                    &env,
                    *id,
                    match &call.kind {
                        ExprKind::IntrinsicCall(_, types, _) => types,
                        _ => unreachable!(),
                    },
                    match &call.kind {
                        ExprKind::IntrinsicCall(_, _, args) => args,
                        _ => unreachable!(),
                    },
                    &request.arguments,
                    span,
                )
                .map_err(E::CallCheck)?
                .kind
            }
            CallableTarget::Source(_) => {
                if let ExprKind::Call(target, _) = &call.kind {
                    match env.get(target.as_ref()) {
                        Some(Type::Function(kind, ..)) => *kind,
                        _ => RoutineKind::Procedure,
                    }
                } else {
                    RoutineKind::Procedure
                }
            }
        };
        let ty = Type::Function(
            kind,
            params.iter().map(|p| (p.ty.clone(), false)).collect(),
            Box::new(result.clone()),
        );
        if let Some(expected) = &request.expected
            && !cyallo_ir::declarations::equivalent_types(expected, &ty)
            && !matches!((expected,&ty),(Type::Function(RoutineKind::Procedure,a,r),Type::Function(RoutineKind::Function,b,s)) if a==b && cyallo_ir::declarations::equivalent_types(r,s))
        {
            return Err(E::Type {
                expected: expected.clone(),
                actual: ty,
            });
        }
        wrapper.kind = kind;
        wrapper.result = result.clone();
        wrapper.body = if result == Type::Void {
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
        } else {
            vec![Stmt {
                kind: StmtKind::Return(call),
                span,
            }]
        };
        let compiled = compiler::compile_ast(&plan.archive, linked, plan.options, true)
            .map_err(E::CallCheck)?;
        let (mut extension, initializer, function) =
            publication::publish(&program, compiled, &name).map_err(compile_error)?;
        let adapter = Adapter {
            request: request.clone(),
            function,
        };
        plan.calls.push((request, adapter.clone()));
        extension.dynamic_plan = Some(Arc::new(plan));
        extension.source_modules = program.source_modules.clone();
        program = Arc::new(extension);
        adapters.push(adapter);
        initializers.extend(initializer);
    }
    Ok(CompiledCalls {
        program,
        adapters,
        initializers,
    })
}
