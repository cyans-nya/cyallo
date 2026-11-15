use super::*;
impl Parser<'_> {
    pub(super) fn word(&mut self, word: &str) -> bool {
        if self.token_text() == word {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    pub(super) fn qualified_name(&mut self) -> Result<Box<str>, Diagnostic> {
        let mut name = if matches!(
            self.token().kind,
            TokenKind::Gc | TokenKind::Rc | TokenKind::Object | TokenKind::Dynamic
        ) {
            let name = self.token_text().to_owned();
            self.pos += 1;
            name
        } else {
            self.name()?.to_string()
        };
        while self.take(":") {
            self.expect(":")?;
            name.push_str("::");
            name.push_str(self.token_text());
            self.pos += 1;
        }
        Ok(name.into())
    }
    fn use_tree(
        &mut self,
        prefix: &str,
        public: bool,
        ast: &mut Ast,
        start: Span,
    ) -> Result<(), Diagnostic> {
        if self.take("{") {
            self.newlines();
            while !self.take("}") {
                self.use_tree(prefix, public, ast, start)?;
                self.newlines();
                if self.take("}") {
                    break;
                }
                self.expect(",")?;
                self.newlines();
            }
            return Ok(());
        }
        let mut path = prefix.to_string();
        let glob = self.take("*");
        if !glob {
            if self.token().kind != TokenKind::Identifier && !self.token().kind.is_keyword() {
                return Err(self.error("expected import path"));
            }
            let segment = self.token_text();
            self.pos += 1;
            if segment != "self" || prefix.is_empty() {
                if !path.is_empty() {
                    path.push_str("::");
                }
                path.push_str(segment);
            }
            if self.take(":") {
                self.expect(":")?;
                return self.use_tree(&path, public, ast, start);
            }
        }
        let alias = if self.word("as") {
            Some(self.name()?)
        } else {
            None
        };
        ast.imports.push(Import {
            path: path.into(),
            alias,
            span: self.finish_span(start),
            public,
            glob,
        });
        Ok(())
    }
    pub(super) fn module_declaration(&mut self, ast: &mut Ast) -> Result<bool, Diagnostic> {
        let start = self.token().span;
        let public = self.word("pub");
        if public && self.take("(") {
            if !self.word("crate") {
                return Err(self.error("expected crate"));
            }
            self.expect(")")?;
        }
        if self.word("type") {
            let name = self.name()?;
            self.expect("=")?;
            let ty = self.ty()?;
            if public {
                ast.exports.push(format!("$type${name}").into());
            }
            ast.type_aliases.push(TypeAlias {
                name,
                ty,
                span: self.finish_span(start),
            });
            self.eol()?;
            return Ok(true);
        }
        if self.word("use") {
            self.use_tree("", public, ast, start)?;
            self.eol()?;
            return Ok(true);
        }
        if self.word("mod") {
            if self.token().kind != TokenKind::Identifier && !self.token().kind.is_keyword() {
                return Err(self.error("expected module name"));
            }
            let name: Box<str> = self.token_text().into();
            self.pos += 1;
            let body = if self.take("{") {
                Some(Box::new(self.declarations(true)?))
            } else {
                None
            };
            ast.modules.push(ModuleDeclaration {
                boundary: Default::default(),
                annotations: vec![],
                name,
                public,
                body,
                span: self.finish_span(start),
            });
            self.eol()?;
            return Ok(true);
        }
        if public && self.token_text() != "property" {
            let ahead = self
                .tokens
                .get(self.pos + 1)
                .copied()
                .ok_or_else(|| self.error("expected declaration"))?;
            let name = self.tokens.text(ahead);
            let namespace = if matches!(self.token_text(), "trait" | "record") {
                "$type$"
            } else {
                ""
            };
            ast.exports.push(format!("{namespace}{name}").into());
        }
        if self.word("trait") {
            let name = self.name()?;
            let type_params = self.type_params()?;
            self.expect("{")?;
            self.newlines();
            let mut methods = vec![];
            while !self.take("}") {
                methods.push(self.modern_routine(true)?);
            }
            ast.traits.push(Trait {
                type_params,
                annotations: vec![],
                origin: TraitOrigin::Source,
                name,
                methods,
                span: self.finish_span(start),
            });
            self.eol()?;
            return Ok(true);
        }
        if self.word("impl") {
            let type_params = self.type_params()?;
            let first = self.ty()?;
            if self.at("{") {
                self.expect("{")?;
                self.newlines();
                let mut methods = vec![];
                while !self.take("}") {
                    let (attributes, annotations) = self.declaration_attributes()?;
                    let public = self.word("pub");
                    let mut function = self.modern_function()?;
                    function.attributes = attributes;
                    function.annotations.extend(annotations);
                    methods.push(InherentMethod { public, function });
                }
                ast.inherent_implementations.push(InherentImplementation {
                    type_params,
                    receiver: first,
                    methods,
                    span: self.finish_span(start),
                });
                self.eol()?;
                return Ok(true);
            }
            if !type_params.is_empty() {
                return Err(self.error("generic trait implementations are not yet supported"));
            }
            let Type::Named(trait_name) = first else {
                return Err(self.error("expected trait name"));
            };
            self.expect("for")?;
            let receiver = self.ty()?;
            self.expect("{")?;
            self.newlines();
            let mut methods = vec![];
            while !self.take("}") {
                methods.push(self.modern_function()?);
            }
            ast.implementations.push(Implementation {
                trait_name,
                receiver,
                methods,
                span: self.finish_span(start),
            });
            self.eol()?;
            return Ok(true);
        }
        if self.word("property") {
            let receiver = self.ty()?;
            self.expect(".")?;
            let name = self.name()?;
            if public {
                ast.exports.push(format!("$property${name}").into());
            }
            self.expect(":")?;
            let result = self.ty()?;
            self.expect("{")?;
            self.newlines();
            let mut getter = self.modern_function()?;
            if getter.name.as_ref() != "get"
                || !getter.params.is_empty()
                || getter.result != Type::Void
            {
                return Err(self.error(
                    "property requires proc get() with its result declared on the property",
                ));
            }
            getter.result = result;
            getter.name = format!("$get${name}").into();
            getter.params.push(Param {
                arity: ParameterArity::One,
                passing: ParameterPassing::Value,
                receiver: true,
                name: "self".into(),
                ty: receiver.clone(),
                by_ref: false,
                span: start,
            });
            self.expect("}")?;
            self.eol()?;
            ast.properties.push(Property {
                receiver,
                name,
                getter,
                span: self.finish_span(start),
            });
            return Ok(true);
        }
        if public {
            return Ok(false);
        }
        Ok(false)
    }
}
