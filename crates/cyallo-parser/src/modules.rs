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
        let mut name = self.name()?.to_string();
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
        if self.word("use") {
            self.use_tree("", public, ast, start)?;
            self.eol()?;
            return Ok(true);
        }
        if self.word("mod") {
            let name = self.name()?;
            let body = if self.take("{") {
                Some(Box::new(self.declarations(true)?))
            } else {
                None
            };
            ast.modules.push(ModuleDeclaration {
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
            self.expect("{")?;
            self.newlines();
            let mut methods = vec![];
            while !self.take("}") {
                methods.push(self.modern_routine(true)?);
            }
            ast.traits.push(Trait {
                origin: TraitOrigin::Source,
                name,
                methods,
                span: self.finish_span(start),
            });
            self.eol()?;
            return Ok(true);
        }
        if self.word("impl") {
            let trait_name = self.qualified_name()?;
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
