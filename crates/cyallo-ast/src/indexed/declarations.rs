//! Declaration and routine syntax in the indexed AST.

use super::*;

struct_syntax!(DeclarationAnnotation => AnnotationNode {
    name: NameId,
    value: NameId,
    span: Span,
});

struct_syntax!(GenericConstraint => ConstraintNode {
    parameter: NameId,
    bounds: List<Id<TypeNode>>,
});

struct_syntax!(Param => ParamNode {
    arity: ParameterArity,
    passing: ParameterPassing,
    receiver: bool,
    name: NameId,
    ty: Id<TypeNode>,
    by_ref: bool,
    span: Span,
});

struct_syntax!(ReferenceCapture => CaptureNode {
    name: NameId,
    mutable: bool,
});

#[derive(Debug)]
pub struct FunctionNode {
    pub annotations: List<AnnotationNode>,
    pub body_kind: FunctionBodyKind,
    pub constraints: List<ConstraintNode>,
    pub type_packs: List<NameId>,
    pub generator: bool,
    pub source_name: NameId,
    pub attributes: RoutineAttributes,
    pub type_params: List<NameId>,
    pub kind: RoutineKind,
    pub name: NameId,
    pub params: List<ParamNode>,
    pub result: Id<TypeNode>,
    pub body: List<Id<StmtNode>>,
    pub span: Span,
}

impl Freeze for Function {
    type Stored = Id<FunctionNode>;

    fn freeze(self, b: &mut Builder) -> IndexResult<Self::Stored> {
        if self.origin != RoutineOrigin::Source {
            return Err(IndexError::SemanticNode(SemanticSyntax::RoutineOrigin));
        }
        with_stack(|| {
            let node = FunctionNode {
                annotations: self.annotations.freeze(b)?,
                body_kind: self.body_kind,
                constraints: self.constraints.freeze(b)?,
                type_packs: self.type_packs.freeze(b)?,
                generator: self.generator,
                source_name: self.source_name.freeze(b)?,
                attributes: self.attributes,
                type_params: self.type_params.freeze(b)?,
                kind: self.kind,
                name: self.name.freeze(b)?,
                params: self.params.freeze(b)?,
                result: self.result.freeze(b)?,
                body: self.body.freeze(b)?,
                span: self.span,
            };
            b.push(node)
        })
    }

    fn thaw(id: &Self::Stored, ast: &IndexedAst) -> Self {
        with_stack(|| {
            let node = ast.node(*id);
            Self {
                annotations: Freeze::thaw(&node.annotations, ast),
                body_kind: node.body_kind,
                constraints: Freeze::thaw(&node.constraints, ast),
                type_packs: Freeze::thaw(&node.type_packs, ast),
                generator: node.generator,
                source_name: Freeze::thaw(&node.source_name, ast),
                origin: RoutineOrigin::Source,
                attributes: node.attributes,
                type_params: Freeze::thaw(&node.type_params, ast),
                kind: node.kind,
                name: Freeze::thaw(&node.name, ast),
                params: Freeze::thaw(&node.params, ast),
                result: Freeze::thaw(&node.result, ast),
                body: Freeze::thaw(&node.body, ast),
                span: node.span,
            }
        })
    }
}

#[derive(Debug)]
pub struct EnumDeclarationNode {
    pub name: NameId,
    pub type_params: List<NameId>,
    pub open: bool,
    pub annotations: List<AnnotationNode>,
    pub variants: List<VariantDeclarationNode>,
    pub span: Span,
}

impl Freeze for EnumDeclaration {
    type Stored = EnumDeclarationNode;

    fn freeze(self, b: &mut Builder) -> IndexResult<Self::Stored> {
        if self.resolved.is_some() {
            return Err(IndexError::SemanticNode(SemanticSyntax::EnumDeclaration));
        }
        Ok(EnumDeclarationNode {
            name: self.name.freeze(b)?,
            type_params: self.type_params.freeze(b)?,
            open: self.open,
            annotations: self.annotations.freeze(b)?,
            variants: self.variants.freeze(b)?,
            span: self.span,
        })
    }

    fn thaw(node: &Self::Stored, ast: &IndexedAst) -> Self {
        Self {
            resolved: None,
            name: Freeze::thaw(&node.name, ast),
            type_params: Freeze::thaw(&node.type_params, ast),
            open: node.open,
            annotations: Freeze::thaw(&node.annotations, ast),
            variants: Freeze::thaw(&node.variants, ast),
            span: node.span,
        }
    }
}

#[derive(Debug)]
pub struct VariantDeclarationNode {
    pub name: NameId,
    pub payload: Option<Id<TypeNode>>,
    pub annotations: List<AnnotationNode>,
    pub span: Span,
}

impl Freeze for VariantDeclaration {
    type Stored = VariantDeclarationNode;

    fn freeze(self, b: &mut Builder) -> IndexResult<Self::Stored> {
        if self.resolved.is_some() {
            return Err(IndexError::SemanticNode(SemanticSyntax::EnumVariant));
        }
        Ok(VariantDeclarationNode {
            name: self.name.freeze(b)?,
            payload: self.payload.freeze(b)?,
            annotations: self.annotations.freeze(b)?,
            span: self.span,
        })
    }

    fn thaw(node: &Self::Stored, ast: &IndexedAst) -> Self {
        Self {
            resolved: None,
            name: Freeze::thaw(&node.name, ast),
            payload: Freeze::thaw(&node.payload, ast),
            annotations: Freeze::thaw(&node.annotations, ast),
            span: node.span,
        }
    }
}

struct_syntax!(TypeAlias => TypeAliasNode {
    name: NameId,
    ty: Id<TypeNode>,
    span: Span,
});

struct_syntax!(Record => RecordNode {
    opaque: bool,
    annotations: List<AnnotationNode>,
    type_params: List<NameId>,
    name: NameId,
    fields: List<(NameId, Id<TypeNode>)>,
    span: Span,
});

struct_syntax!(Import => ImportNode {
    path: NameId,
    alias: Option<NameId>,
    span: Span,
    public: bool,
    glob: bool,
});

#[derive(Debug)]
pub struct TraitNode {
    pub type_params: List<NameId>,
    pub annotations: List<AnnotationNode>,
    pub name: NameId,
    pub methods: List<Id<FunctionNode>>,
    pub span: Span,
}

impl Freeze for Trait {
    type Stored = TraitNode;

    fn freeze(self, b: &mut Builder) -> IndexResult<Self::Stored> {
        if self.origin != TraitOrigin::Source {
            return Err(IndexError::SemanticNode(SemanticSyntax::TraitOrigin));
        }
        Ok(TraitNode {
            type_params: self.type_params.freeze(b)?,
            annotations: self.annotations.freeze(b)?,
            name: self.name.freeze(b)?,
            methods: self.methods.freeze(b)?,
            span: self.span,
        })
    }

    fn thaw(node: &Self::Stored, ast: &IndexedAst) -> Self {
        Self {
            type_params: Freeze::thaw(&node.type_params, ast),
            annotations: Freeze::thaw(&node.annotations, ast),
            origin: TraitOrigin::Source,
            name: Freeze::thaw(&node.name, ast),
            methods: Freeze::thaw(&node.methods, ast),
            span: node.span,
        }
    }
}

struct_syntax!(Implementation => ImplementationNode {
    trait_name: NameId,
    receiver: Id<TypeNode>,
    methods: List<Id<FunctionNode>>,
    span: Span,
});

struct_syntax!(InherentMethod => InherentMethodNode {
    public: bool,
    function: Id<FunctionNode>,
});

struct_syntax!(InherentImplementation => InherentImplementationNode {
    type_params: List<NameId>,
    receiver: Id<TypeNode>,
    methods: List<InherentMethodNode>,
    span: Span,
});

struct_syntax!(Property => PropertyNode {
    receiver: Id<TypeNode>,
    name: NameId,
    getter: Id<FunctionNode>,
    span: Span,
});

struct_syntax!(InterfaceImplementation => InterfaceImplementationNode {
    interface: NameId,
    module: NameId,
    bindings: List<(NameId, Id<TypeNode>)>,
    span: Span,
});

struct_syntax!(World => WorldNode {
    annotations: List<AnnotationNode>,
    name: NameId,
    public: bool,
    items: List<WorldItemNode>,
    span: Span,
});

#[derive(Debug)]
pub enum WorldItemNode {
    Include {
        path: NameId,
        span: Span,
    },
    Import {
        path: NameId,
        alias: Option<NameId>,
        members: Option<List<Id<FunctionNode>>>,
        span: Span,
    },
    Export {
        function: Id<FunctionNode>,
    },
}

impl Freeze for WorldItem {
    type Stored = WorldItemNode;

    fn freeze(self, b: &mut Builder) -> IndexResult<Self::Stored> {
        Ok(match self {
            Self::Include { path, span } => WorldItemNode::Include {
                path: path.freeze(b)?,
                span,
            },
            Self::Import {
                path,
                alias,
                members,
                span,
            } => WorldItemNode::Import {
                path: path.freeze(b)?,
                alias: alias.freeze(b)?,
                members: members.freeze(b)?,
                span,
            },
            Self::Export { function } => WorldItemNode::Export {
                function: function.freeze(b)?,
            },
        })
    }

    fn thaw(node: &Self::Stored, ast: &IndexedAst) -> Self {
        match node {
            WorldItemNode::Include { path, span } => Self::Include {
                path: Freeze::thaw(path, ast),
                span: *span,
            },
            WorldItemNode::Import {
                path,
                alias,
                members,
                span,
            } => Self::Import {
                path: Freeze::thaw(path, ast),
                alias: Freeze::thaw(alias, ast),
                members: Freeze::thaw(members, ast),
                span: *span,
            },
            WorldItemNode::Export { function } => Self::Export {
                function: Freeze::thaw(function, ast),
            },
        }
    }
}

#[derive(Debug)]
pub struct ModuleDeclarationNode {
    pub interface: bool,
    pub annotations: List<AnnotationNode>,
    pub name: NameId,
    pub public: bool,
    pub body: Option<Id<ModuleNode>>,
    pub span: Span,
}

impl Freeze for ModuleDeclaration {
    type Stored = ModuleDeclarationNode;

    fn freeze(self, b: &mut Builder) -> IndexResult<Self::Stored> {
        if self.boundary != ModuleBoundary::Lexical {
            return Err(IndexError::SemanticNode(SemanticSyntax::PackageBoundary));
        }
        Ok(ModuleDeclarationNode {
            interface: self.interface,
            annotations: self.annotations.freeze(b)?,
            name: self.name.freeze(b)?,
            public: self.public,
            body: self.body.freeze(b)?,
            span: self.span,
        })
    }

    fn thaw(node: &Self::Stored, ast: &IndexedAst) -> Self {
        Self {
            interface: node.interface,
            boundary: ModuleBoundary::Lexical,
            annotations: Freeze::thaw(&node.annotations, ast),
            name: Freeze::thaw(&node.name, ast),
            public: node.public,
            body: Freeze::thaw(&node.body, ast),
            span: node.span,
        }
    }
}

#[derive(Debug)]
pub struct ModuleNode {
    pub enums: List<EnumDeclarationNode>,
    pub type_aliases: List<TypeAliasNode>,
    pub body: List<Id<StmtNode>>,
    pub functions: List<Id<FunctionNode>>,
    pub records: List<RecordNode>,
    pub imports: List<ImportNode>,
    pub exports: List<NameId>,
    pub traits: List<TraitNode>,
    pub implementations: List<ImplementationNode>,
    pub properties: List<PropertyNode>,
    pub modules: List<ModuleDeclarationNode>,
    pub inherent_implementations: List<InherentImplementationNode>,
    pub worlds: List<WorldNode>,
    pub interface_implementations: List<InterfaceImplementationNode>,
    /// Original spellings of case-folded compatibility identifiers.
    pub source_names: List<(NameId, NameId)>,
}

impl Freeze for Ast {
    type Stored = Id<ModuleNode>;

    fn freeze(self, b: &mut Builder) -> IndexResult<Self::Stored> {
        if !self.resolved_modules.is_empty()
            || self.source_names.iter().any(|(key, spelling)| {
                !key.bytes()
                    .eq(spelling.bytes().map(|b| b.to_ascii_uppercase()))
            })
        {
            return Err(IndexError::SemanticNode(SemanticSyntax::ModuleIdentity));
        }
        let NativeDeclarations {
            enums,
            import_policy,
            types,
            functions,
            methods,
            traits,
            families,
        } = &self.native;
        if import_policy.is_some()
            || !enums.is_empty()
            || !types.is_empty()
            || !functions.is_empty()
            || !methods.is_empty()
            || !traits.is_empty()
            || !families.is_empty()
        {
            return Err(IndexError::SemanticNode(SemanticSyntax::NativeDeclaration));
        }

        with_stack(|| {
            let node = ModuleNode {
                enums: self.enums.freeze(b)?,
                type_aliases: self.type_aliases.freeze(b)?,
                body: self.body.freeze(b)?,
                functions: self.functions.freeze(b)?,
                records: self.records.freeze(b)?,
                imports: self.imports.freeze(b)?,
                exports: self.exports.freeze(b)?,
                traits: self.traits.freeze(b)?,
                implementations: self.implementations.freeze(b)?,
                properties: self.properties.freeze(b)?,
                modules: self.modules.freeze(b)?,
                inherent_implementations: self.inherent_implementations.freeze(b)?,
                worlds: self.worlds.freeze(b)?,
                interface_implementations: self.interface_implementations.freeze(b)?,
                source_names: self
                    .source_names
                    .into_iter()
                    .collect::<Vec<_>>()
                    .freeze(b)?,
            };
            b.push(node)
        })
    }

    fn thaw(id: &Self::Stored, ast: &IndexedAst) -> Self {
        with_stack(|| {
            let node = ast.node(*id);
            Self {
                enums: Freeze::thaw(&node.enums, ast),
                type_aliases: Freeze::thaw(&node.type_aliases, ast),
                resolved_modules: Vec::new(),
                native: NativeDeclarations::default(),
                source_names: Vec::<(Box<str>, Box<str>)>::thaw(&node.source_names, ast)
                    .into_iter()
                    .collect(),
                body: Freeze::thaw(&node.body, ast),
                functions: Freeze::thaw(&node.functions, ast),
                records: Freeze::thaw(&node.records, ast),
                imports: Freeze::thaw(&node.imports, ast),
                exports: Freeze::thaw(&node.exports, ast),
                traits: Freeze::thaw(&node.traits, ast),
                implementations: Freeze::thaw(&node.implementations, ast),
                properties: Freeze::thaw(&node.properties, ast),
                modules: Freeze::thaw(&node.modules, ast),
                inherent_implementations: Freeze::thaw(&node.inherent_implementations, ast),
                worlds: Freeze::thaw(&node.worlds, ast),
                interface_implementations: Freeze::thaw(&node.interface_implementations, ast),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_conversion_rejects_a_package_boundary_in_nested_declarations() {
        let tree = Ast {
            modules: vec![ModuleDeclaration {
                interface: false,
                boundary: ModuleBoundary::Lexical,
                annotations: Vec::new(),
                name: "outer".into(),
                public: false,
                body: Some(Box::new(Ast {
                    modules: vec![ModuleDeclaration {
                        interface: false,
                        boundary: ModuleBoundary::Package,
                        annotations: Vec::new(),
                        name: "nested_package".into(),
                        public: false,
                        body: None,
                        span: SPAN,
                    }],
                    ..Ast::default()
                })),
                span: SPAN,
            }],
            ..Ast::default()
        };
        assert_eq!(
            IndexedAst::from_source_tree(tree).unwrap_err(),
            IndexError::SemanticNode(SemanticSyntax::PackageBoundary),
        );
    }
}
