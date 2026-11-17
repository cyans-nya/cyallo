use super::*;
impl Specializer {
    pub(super) fn enumeration(
        &mut self,
        name: &str,
        args: &[Type],
        span: Span,
    ) -> Result<Box<str>> {
        let args: Vec<_> = args.iter().map(canonical).collect();
        let id = format!("$generic${}<{args:?}>", key(name));
        if self.record_arguments.contains_key(&id) {
            return Ok(id.into());
        }
        let mut declaration = self.enumerations.get(name).cloned().ok_or_else(|| {
            crate::enums::invalid_enum(
                crate::enums::EnumError::UnresolvedDeclaration { name: name.into() },
                span,
            )
        })?;
        let bindings = Self::bindings(&declaration.type_params, &args, span)?;
        self.reserve(span)?;
        self.record_arguments
            .insert(id.clone(), (name.into(), args.clone()));
        let resolved = declaration.resolved.as_mut().ok_or_else(|| {
            crate::enums::invalid_enum(
                crate::enums::EnumError::UnresolvedDeclaration { name: name.into() },
                span,
            )
        })?;
        resolved.arguments = args;
        declaration.type_params.clear();
        declaration.name = id.clone().into();
        for variant in &mut declaration.variants {
            if let Some(payload) = &mut variant.payload {
                substitute(payload, &bindings);
                self.ty(payload, variant.span)?;
            }
        }
        if let Some(&owner) = self.owners.get(name) {
            self.owners.insert(id, owner);
        }
        let result = declaration.name.clone();
        self.generated_enums.push(declaration);
        Ok(result)
    }
    pub(super) fn constructor_context(
        &mut self,
        body: &mut [Stmt],
        returned: Option<&Type>,
    ) -> Result<()> {
        cyallo_ast::with_stack(|| {
            for statement in body {
                match &mut statement.kind {
                    StmtKind::Binding(_, annotation, _, _, value) => {
                        self.context_expression(value, annotation.as_ref())?
                    }
                    StmtKind::Return(value) => self.context_expression(value, returned)?,
                    StmtKind::Evaluate(value) | StmtKind::Constant(_, value) => {
                        self.context_expression(value, None)?
                    }
                    StmtKind::If(value, yes, no) | StmtKind::IfLet(_, value, yes, no) => {
                        self.context_expression(value, None)?;
                        self.constructor_context(yes, returned)?;
                        self.constructor_context(no, returned)?;
                    }
                    StmtKind::While(value, body)
                    | StmtKind::Repeat(body, value)
                    | StmtKind::ForIn(_, value, body) => {
                        self.context_expression(value, None)?;
                        self.constructor_context(body, returned)?;
                    }
                    StmtKind::For(_, first, last, step, body) => {
                        self.context_expression(first, None)?;
                        self.context_expression(last, None)?;
                        if let Some(step) = step {
                            self.context_expression(step, None)?;
                        }
                        self.constructor_context(body, returned)?;
                    }
                    StmtKind::Case(value, arms, otherwise) => {
                        self.context_expression(value, None)?;
                        for (_, body) in arms {
                            self.constructor_context(body, returned)?;
                        }
                        self.constructor_context(otherwise, returned)?;
                    }
                    StmtKind::Assign(target, value) | StmtKind::Compound(target, _, value) => {
                        self.context_expression(target, None)?;
                        self.context_expression(value, None)?;
                    }
                    StmtKind::Output(values) | StmtKind::Call(_, values) => {
                        for value in values {
                            self.context_expression(value, None)?;
                        }
                    }
                    _ => {}
                }
            }
            Ok(())
        })
    }
    pub(super) fn context_expression(
        &mut self,
        value: &mut Expr,
        expected: Option<&Type>,
    ) -> Result<()> {
        cyallo_ast::with_stack(|| {
            match &mut value.kind {
                ExprKind::Name(name) => {
                    if let Some(template) = self
                        .functions
                        .get(name.as_ref())
                        .filter(|f| f.origin == RoutineOrigin::EnumConstructor)
                        .cloned()
                        && let Some(expected) = expected
                    {
                        let mut bindings = HashMap::new();
                        if self.unify(
                            &crate::composites::function_type(&template),
                            expected,
                            &template.type_params,
                            &mut bindings,
                        ) && let Some(arguments) = template
                            .type_params
                            .iter()
                            .map(|p| bindings.get(p.as_ref()).cloned())
                            .collect::<Option<Vec<_>>>()
                        {
                            *name = self.function(name, &arguments, value.span)?;
                        }
                    }
                }
                ExprKind::Call(name, args) => {
                    let template = self
                        .functions
                        .get(name.as_ref())
                        .filter(|f| f.origin == RoutineOrigin::EnumConstructor)
                        .cloned();
                    let bindings =
                        template
                            .as_ref()
                            .zip(expected)
                            .and_then(|(template, expected)| {
                                let mut bindings = HashMap::new();
                                self.unify(
                                    &template.result,
                                    expected,
                                    &template.type_params,
                                    &mut bindings,
                                )
                                .then_some(bindings)
                            });
                    let arguments = template.as_ref().zip(bindings.as_ref()).and_then(
                        |(template, bindings)| {
                            template
                                .type_params
                                .iter()
                                .map(|p| bindings.get(p.as_ref()).cloned())
                                .collect::<Option<Vec<_>>>()
                        },
                    );
                    if let (Some(template), Some(bindings), Some(arguments)) =
                        (template, bindings, arguments)
                    {
                        for (argument, parameter) in args.iter_mut().zip(&template.params) {
                            let mut ty = parameter.ty.clone();
                            substitute(&mut ty, &bindings);
                            self.context_expression(argument, Some(&ty))?;
                        }
                        let mut arguments = arguments;
                        for argument in &mut arguments {
                            self.ty(argument, value.span)?;
                        }
                        let instance = self.function(name, &arguments, value.span)?;
                        value.kind = ExprKind::Call(instance, std::mem::take(args));
                    } else {
                        for arg in args {
                            self.context_expression(arg, None)?;
                        }
                    }
                }
                ExprKind::GenericCall(_, _, args)
                | ExprKind::IntrinsicCall(_, _, args)
                | ExprKind::TypedCollection(_, _, args)
                | ExprKind::ArrayLiteral(args) => {
                    for arg in args {
                        self.context_expression(arg, None)?;
                    }
                }
                ExprKind::Tuple(values) => {
                    let expected = match expected {
                        Some(Type::Tuple(types)) => Some(types),
                        _ => None,
                    };
                    for (i, value) in values.iter_mut().enumerate() {
                        self.context_expression(value, expected.and_then(|ts| ts.get(i)))?;
                    }
                }
                ExprKind::AnonymousRecord(fields)
                | ExprKind::VariantPayload(fields)
                | ExprKind::Record(_, fields)
                | ExprKind::GenericRecord(_, _, fields)
                | ExprKind::ObjectLiteral(_, fields) => {
                    let declared = match expected {
                        Some(
                            Type::AnonymousRecord(fields)
                            | Type::Record(_, fields)
                            | Type::Object(fields),
                        ) => Some(fields),
                        _ => None,
                    };
                    for (name, value) in fields {
                        self.context_expression(
                            value,
                            declared
                                .and_then(|fs| fs.iter().find(|(n, _)| n == name).map(|(_, t)| t)),
                        )?;
                    }
                }
                ExprKind::Closure(function, _) => {
                    self.constructor_context(&mut function.body, Some(&function.result))?
                }
                ExprKind::Cast(value, ty) => self.context_expression(value, Some(ty))?,
                ExprKind::Parenthesized(value) => self.context_expression(value, expected)?,
                ExprKind::Match(value, arms) => {
                    self.context_expression(value, None)?;
                    for arm in arms {
                        if let Some(guard) = &mut arm.guard {
                            self.context_expression(guard, None)?;
                        }
                        match &mut arm.body {
                            MatchBody::Expression(value) => {
                                self.context_expression(value, expected)?
                            }
                            MatchBody::Block(body, tail) => {
                                self.constructor_context(body, None)?;
                                if let Some(tail) = tail {
                                    self.context_expression(tail, expected)?;
                                }
                            }
                        }
                    }
                }
                ExprKind::Binary(_, a, b) | ExprKind::NativeCoerce(a, b) => {
                    self.context_expression(a, None)?;
                    self.context_expression(b, None)?;
                }
                ExprKind::Invoke(value, args) | ExprKind::Index(value, args) => {
                    self.context_expression(value, None)?;
                    for arg in args {
                        self.context_expression(arg, None)?;
                    }
                }
                ExprKind::ManagedNew(_, _, allocator, value) => {
                    if let Some(allocator) = allocator {
                        self.context_expression(allocator, None)?;
                    }
                    let expected = match expected {
                        Some(Type::Managed { pointee, .. }) => Some(pointee.as_ref()),
                        _ => None,
                    };
                    self.context_expression(value, expected)?;
                }
                ExprKind::DeleteProperty(value)
                | ExprKind::Yield(value, _)
                | ExprKind::Field(value, _)
                | ExprKind::TupleField(value, _)
                | ExprKind::ExtensionField(value, _, _)
                | ExprKind::Unary(_, value) => self.context_expression(value, None)?,
                ExprKind::EnumConstruct(_, _, Some(payload)) => {
                    self.context_expression(payload, None)?
                }
                ExprKind::ClosureValue(_, captures, _) => {
                    for value in captures {
                        self.context_expression(value, None)?;
                    }
                }
                _ => {}
            }
            Ok(())
        })
    }
}
