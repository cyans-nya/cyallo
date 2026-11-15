//! Lexical, case-sensitive resolution shared by single-source and module builds.
use crate::declarations::{
    self, DeclarationError, DeclarationKind, SourceTrust, invalid_declaration,
};
use crate::internal::{InternalSemantic, NativeType};
use crate::intrinsic::Intrinsic;
use cyallo_ast::*;
use std::collections::HashMap;
use std::path::Path;
type Result<T> = std::result::Result<T, Diagnostic>;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind {
    Module,
    Type,
    Value,
    Property,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SymbolOrigin {
    Source,
    Core,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Symbol {
    id: DeclarationId,
    native: Option<InternalSemantic>,
    origin: SymbolOrigin,
    target: Box<str>,
    kind: Kind,
    owner: usize,
    defining_module: usize,
    public: bool,
    intrinsic: Option<Intrinsic>,
}
#[derive(Clone, Copy)]
struct Placement {
    trust: SourceTrust,
    parent: Option<usize>,
    boundary: ModuleBoundary,
}
struct Node {
    parent: Option<usize>,
    package: usize,
    id: DeclarationId,
    trust: SourceTrust,
    path: String,
    ast: Ast,
    names: HashMap<String, Vec<Symbol>>,
    bindings: HashMap<u32, Box<str>>,
}
#[derive(Clone)]
struct Local {
    target: Box<str>,
    writable: bool,
}
struct Resolver {
    aliases: HashMap<Box<str>, (usize, Type)>,
    source_names: HashMap<Box<str>, Box<str>>,
    nodes: Vec<Node>,
    next_binding: usize,
    next_declaration: u32,
    prelude: Option<usize>,
    packages: HashMap<String, usize>,
    associated: HashMap<DeclarationId, HashMap<String, Vec<Symbol>>>,
    associated_functions: Vec<(usize, NativeFunctionDeclaration)>,
    permissions: Option<crate::import_policy::ResolvedPolicy>,
    requesting_modules: HashMap<u32, DeclarationId>,
}
fn error(message: impl Into<String>, span: Span) -> Diagnostic {
    Diagnostic::new("E140", message, span)
}
impl Resolver {
    fn declaration_id(&mut self) -> DeclarationId {
        let id = DeclarationId(self.next_declaration);
        self.next_declaration += 1;
        id
    }
    fn add(
        &mut self,
        path: String,
        mut ast: Ast,
        files: &mut [Option<Ast>],
        paths: &[String],
        dir: &Path,
        placement: Placement,
    ) -> Result<usize> {
        let trust = placement.trust;
        if self.nodes.iter().any(|n| n.path == path) {
            return Err(error("duplicate module", Span::default()));
        }
        let index = self.nodes.len();
        let id = self.declaration_id();
        let declarations = std::mem::take(&mut ast.modules);
        let package = if placement.boundary == ModuleBoundary::Package {
            index
        } else {
            placement
                .parent
                .map_or(index, |parent| self.nodes[parent].package)
        };
        self.nodes.push(Node {
            parent: placement.parent,
            package,
            id,
            trust,
            path: path.clone(),
            ast,
            names: HashMap::new(),
            bindings: HashMap::new(),
        });
        for declaration in declarations {
            let child_path = format!("{path}::{}", declaration.name);
            let child_dir = dir.join(declaration.name.as_ref());
            let body = if let Some(body) = declaration.body {
                *body
            } else {
                let candidates = [
                    dir.join(format!("{}.ya", declaration.name)),
                    child_dir.join("mod.ya"),
                ];
                let matches: Vec<_> = paths
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| candidates.iter().any(|c| Path::new(p) == c))
                    .map(|(i, _)| i)
                    .collect();
                if matches.len() != 1 {
                    return Err(error(
                        format!(
                            "expected exactly one source for module {}",
                            declaration.name
                        ),
                        declaration.span,
                    ));
                }
                files[matches[0]].take().ok_or_else(|| {
                    error(
                        "module source is already owned by another module",
                        declaration.span,
                    )
                })?
            };
            let semantic =
                declarations::bind(&declaration.annotations, trust, DeclarationKind::Module)?;
            let child = self.add(
                child_path.clone(),
                body,
                files,
                paths,
                &child_dir,
                Placement {
                    trust,
                    parent: Some(index),
                    boundary: declaration.boundary,
                },
            )?;
            if semantic == Some(InternalSemantic::Prelude) && self.prelude.replace(child).is_some()
            {
                return Err(invalid_declaration(
                    DeclarationError::DuplicateCanonical,
                    declaration.span,
                ));
            }
            let child_id = self.nodes[child].id;
            self.nodes[index]
                .names
                .entry(declaration.name.to_string())
                .or_default()
                .push(Symbol {
                    id: child_id,
                    native: semantic,
                    origin: if trust == SourceTrust::Core {
                        SymbolOrigin::Core
                    } else {
                        SymbolOrigin::Source
                    },
                    target: child_path.into(),
                    kind: Kind::Module,
                    owner: index,
                    defining_module: child,
                    public: declaration.public,
                    intrinsic: None,
                });
        }
        Ok(index)
    }
    fn declarations(&mut self, start: usize) -> Result<()> {
        let mut canonical = HashMap::new();
        let mut next_declaration = self.next_declaration;
        for (index, node) in self.nodes.iter_mut().enumerate().skip(start) {
            let mut definitions: Vec<(Box<str>, Kind, bool)> = node
                .ast
                .functions
                .iter()
                .map(|f| {
                    (
                        f.name.clone(),
                        Kind::Value,
                        node.ast.exports.contains(&f.name),
                    )
                })
                .collect();
            definitions.extend(node.ast.type_aliases.iter().map(|alias| {
                (
                    alias.name.clone(),
                    Kind::Type,
                    node.ast
                        .exports
                        .contains(&format!("$type${}", alias.name).into_boxed_str()),
                )
            }));
            definitions.extend(node.ast.records.iter().map(|r| {
                (
                    r.name.clone(),
                    Kind::Type,
                    node.ast
                        .exports
                        .iter()
                        .any(|e| e.as_ref() == format!("$type${}", r.name)),
                )
            }));
            definitions.extend(node.ast.traits.iter().map(|t| {
                (
                    t.name.clone(),
                    Kind::Type,
                    node.ast
                        .exports
                        .iter()
                        .any(|e| e.as_ref() == format!("$type${}", t.name)),
                )
            }));
            definitions.extend(node.ast.properties.iter().map(|p| {
                (
                    p.name.clone(),
                    Kind::Property,
                    node.ast
                        .exports
                        .iter()
                        .any(|e| e.as_ref() == format!("$property${}", p.name)),
                )
            }));
            definitions.extend(node.ast.body.iter().filter_map(|s| match &s.kind {
                StmtKind::Constant(n, _) | StmtKind::Declare(n, _) => {
                    Some((n.clone(), Kind::Value, node.ast.exports.contains(n)))
                }
                _ => None,
            }));
            for (name, kind, public) in definitions {
                let (annotations, declaration_kind, span) = match kind {
                    Kind::Value => node
                        .ast
                        .functions
                        .iter()
                        .find(|f| f.name == name)
                        .map(|f| (&f.annotations[..], DeclarationKind::Function, f.span)),
                    Kind::Type => node
                        .ast
                        .records
                        .iter()
                        .find(|r| r.name == name)
                        .map(|r| (&r.annotations[..], DeclarationKind::Record, r.span))
                        .or_else(|| {
                            node.ast
                                .traits
                                .iter()
                                .find(|t| t.name == name)
                                .map(|t| (&t.annotations[..], DeclarationKind::Trait, t.span))
                        }),
                    _ => None,
                }
                .unwrap_or((&[], DeclarationKind::Function, Span::default()));
                let semantic = declarations::bind(annotations, node.trust, declaration_kind)?;
                if let Some(
                    semantic @ (InternalSemantic::Type(_)
                    | InternalSemantic::TypeFamily(_)
                    | InternalSemantic::Trait(_)
                    | InternalSemantic::AllocatorTrait),
                ) = semantic
                    && let Some(previous) = canonical.insert(semantic, span)
                {
                    return Err(
                        invalid_declaration(DeclarationError::DuplicateCanonical, span)
                            .with_related(previous, "first declaration"),
                    );
                }
                if let Some(record) = node.ast.records.iter().find(|r| r.name == name) {
                    let expected = match semantic {
                        Some(InternalSemantic::Type(t)) => Some(declarations::type_arity(t)),
                        Some(InternalSemantic::TypeFamily(f)) => {
                            Some(declarations::family_arity(f))
                        }
                        _ => None,
                    };
                    if let Some(expected) = expected {
                        if !record.fields.is_empty() {
                            return Err(invalid_declaration(
                                DeclarationError::OpaqueFields,
                                record.span,
                            ));
                        }
                        if expected != record.type_params.len() {
                            return Err(invalid_declaration(
                                DeclarationError::GenericArity {
                                    expected,
                                    actual: record.type_params.len(),
                                },
                                record.span,
                            ));
                        }
                    }
                }
                if let Some(function) = node.ast.functions.iter().find(|f| f.name == name) {
                    if function.body_kind == FunctionBodyKind::Source
                        && (function.params.iter().any(|param| {
                            param.passing != ParameterPassing::Value
                                || param.arity != ParameterArity::One
                        }) || !function.type_packs.is_empty())
                    {
                        return Err(invalid_declaration(
                            DeclarationError::InvalidPack,
                            function.span,
                        ));
                    }
                    match (semantic, function.body_kind) {
                        (Some(_), FunctionBodyKind::Source) => {
                            return Err(invalid_declaration(
                                DeclarationError::NativeBodyForbidden,
                                function.span,
                            ));
                        }
                        (None, FunctionBodyKind::Declaration) => {
                            return Err(invalid_declaration(
                                DeclarationError::BodyRequired,
                                function.span,
                            ));
                        }
                        _ => {}
                    }
                }
                let id = DeclarationId(next_declaration);
                next_declaration += 1;
                let entries = node.names.entry(name.to_string()).or_default();
                if entries.iter().any(|s| s.kind == kind) {
                    return Err(error(
                        format!("duplicate declaration `{}`", cyallo_ast::source_name(&name)),
                        Span::default(),
                    ));
                }
                let target: Box<str> = format!("$module{index}${kind:?}${name}").into();
                self.source_names.insert(target.clone(), name.clone());
                if let Some(alias) = node
                    .ast
                    .type_aliases
                    .iter()
                    .find(|alias| alias.name == name)
                {
                    self.aliases
                        .insert(target.clone(), (index, alias.ty.clone()));
                }
                entries.push(Symbol {
                    id,
                    native: semantic,
                    origin: if node.trust == SourceTrust::Core {
                        SymbolOrigin::Core
                    } else {
                        SymbolOrigin::Source
                    },
                    target,
                    kind,
                    owner: index,
                    defining_module: index,
                    public,
                    intrinsic: match semantic {
                        Some(InternalSemantic::Function(id)) => Some(id),
                        _ => None,
                    },
                });
            }
            let mut versions: HashMap<Box<str>, (usize, bool)> = HashMap::new();
            for statement in &node.ast.body {
                let StmtKind::Binding(name, _, constant, _, _) = &statement.kind else {
                    continue;
                };
                let version = versions.entry(name.clone()).or_insert((0, *constant));
                let entries = node.names.entry(name.to_string()).or_default();
                if (*constant || version.1 || version.0 == 0)
                    && entries.iter().any(|symbol| symbol.kind == Kind::Value)
                {
                    return Err(error(
                        format!("duplicate declaration `{}`", cyallo_ast::source_name(name)),
                        statement.span,
                    ));
                }
                let target: Box<str> = if version.0 == 0 {
                    format!("$module{index}$Value${name}").into()
                } else {
                    format!("$module{index}$Value${name}$shadow{}", version.0).into()
                };
                version.0 += 1;
                self.source_names.insert(target.clone(), name.clone());
                node.bindings.insert(statement.span.start, target.clone());
                entries.retain(|symbol| symbol.kind != Kind::Value);
                entries.push(Symbol {
                    id: DeclarationId(next_declaration),
                    native: None,
                    origin: SymbolOrigin::Source,
                    target,
                    kind: Kind::Value,
                    owner: index,
                    defining_module: index,
                    public: node.ast.exports.contains(name),
                    intrinsic: None,
                });
                next_declaration += 1;
            }
        }
        self.next_declaration = next_declaration;
        self.associated_declarations(start)?;
        Ok(())
    }
    fn associated_declarations(&mut self, start: usize) -> Result<()> {
        for module in start..self.nodes.len() {
            let implementations = self.nodes[module].ast.inherent_implementations.clone();
            for implementation in implementations {
                let owner_name = match &implementation.receiver {
                    Type::Named(n) | Type::Applied(n, _) => Some(n.as_ref()),
                    _ => None,
                };
                let owner = owner_name.and_then(|name| self.path(module, name, Some(Kind::Type)));
                for method in implementation.methods {
                    if method
                        .function
                        .params
                        .first()
                        .is_some_and(|param| param.receiver)
                    {
                        continue;
                    }
                    let owner = owner.as_ref().ok_or_else(|| {
                        invalid_declaration(DeclarationError::UnresolvedOwner, implementation.span)
                    })?;
                    let semantic = declarations::bind(
                        &method.function.annotations,
                        self.nodes[module].trust,
                        DeclarationKind::Member,
                    )?;
                    let id = self.declaration_id();
                    let mut function = method.function;
                    let name = function.name.clone();
                    // Source positions remain stable when independent packages
                    // append nodes; allocation-order IDs do not.
                    let target: Box<str> =
                        format!("$associated{}${name}", function.span.start).into();
                    function.name = target.clone();
                    for parameter in &mut function.params {
                        cyallo_ast::visit::visit_type(&mut parameter.ty, &mut |ty| {
                            if *ty == Type::SelfType {
                                *ty = implementation.receiver.clone();
                            }
                        });
                    }
                    cyallo_ast::visit::visit_type(&mut function.result, &mut |ty| {
                        if *ty == Type::SelfType {
                            *ty = implementation.receiver.clone();
                        }
                    });
                    let mut parameters = implementation.type_params.clone();
                    parameters.extend(function.type_params);
                    function.type_params = parameters;
                    self.source_names.insert(target.clone(), name.clone());
                    self.associated
                        .entry(owner.id)
                        .or_default()
                        .entry(name.to_string())
                        .or_default()
                        .push(Symbol {
                            id,
                            native: semantic,
                            origin: owner.origin,
                            target,
                            kind: Kind::Value,
                            owner: module,
                            defining_module: module,
                            public: method.public,
                            intrinsic: match semantic {
                                Some(InternalSemantic::Function(id)) => Some(id),
                                _ => None,
                            },
                        });
                    self.associated_functions.push((
                        module,
                        NativeFunctionDeclaration {
                            id,
                            module: Some(self.nodes[module].id),
                            function,
                        },
                    ));
                }
            }
        }
        Ok(())
    }
    fn parent_in_package(&self, module: usize) -> Option<usize> {
        (module != self.nodes[module].package)
            .then_some(self.nodes[module].parent)
            .flatten()
    }
    fn accessible(&self, from: usize, symbol: &Symbol) -> bool {
        if symbol.public {
            return true;
        }
        let mut module = Some(from);
        while let Some(index) = module {
            if index == symbol.owner {
                return true;
            }
            module = self.parent_in_package(index);
        }
        false
    }
    fn module_symbol(&self, index: usize) -> Symbol {
        let node = &self.nodes[index];
        Symbol {
            id: node.id,
            native: None,
            origin: if node.trust == SourceTrust::Core {
                SymbolOrigin::Core
            } else {
                SymbolOrigin::Source
            },
            target: node.path.clone().into(),
            kind: Kind::Module,
            owner: index,
            defining_module: index,
            public: true,
            intrinsic: None,
        }
    }
    fn native_target(&self, target: &str) -> Option<(DeclarationId, InternalSemantic)> {
        self.nodes
            .iter()
            .flat_map(|node| node.names.values().flatten())
            .find(|symbol| symbol.target.as_ref() == target && symbol.native.is_some())
            .map(|symbol| (symbol.id, symbol.native.unwrap()))
    }
    fn check_import(&self, from: usize, symbol: &Symbol, span: Span) -> Result<()> {
        if let Some(permissions) = &self.permissions {
            permissions
                .check(self.nodes[from].id, self.nodes[symbol.defining_module].id)
                .map_err(|error| crate::import_policy::import_policy_diagnostic(error, span))?;
        }
        Ok(())
    }
    fn module_graph(&self) -> Vec<crate::import_policy::ModuleDescriptor> {
        self.nodes
            .iter()
            .map(|node| crate::import_policy::ModuleDescriptor {
                id: node.id,
                path: node.path.clone().into(),
                parent: node.parent.map(|index| self.nodes[index].id),
                trust: node.trust,
            })
            .collect()
    }
    fn path(&self, from: usize, path: &str, kind: Option<Kind>) -> Option<Symbol> {
        let parts: Vec<_> = path.split("::").collect();
        let select = |symbols: &[Symbol], requested: Option<Kind>| {
            let matches: Vec<_> = symbols
                .iter()
                .filter(|s| requested.is_none_or(|k| s.kind == k))
                .collect();
            (matches.len() == 1).then(|| matches[0].clone())
        };
        let mut position = 1;
        let mut symbol = match parts[0] {
            "crate" => self.module_symbol(self.nodes[from].package),
            "self" => self.module_symbol(from),
            "super" => self.module_symbol(self.parent_in_package(from)?),
            first => {
                if let Some(symbols) = self.nodes[from].names.get(first) {
                    select(symbols, if parts.len() == 1 { kind } else { None })?
                } else if let Some(root) = self.packages.get(first) {
                    self.module_symbol(*root)
                } else {
                    let symbols = self.nodes[self.prelude?].names.get(first)?;
                    let symbol = select(symbols, if parts.len() == 1 { kind } else { None })?;
                    if !symbol.public {
                        return None;
                    }
                    symbol
                }
            }
        };
        while position < parts.len() {
            let part = parts[position];
            position += 1;
            if symbol.kind == Kind::Module {
                let owner = self
                    .nodes
                    .iter()
                    .position(|n| n.path == symbol.target.as_ref())?;
                if part == "super" {
                    symbol = self.module_symbol(self.parent_in_package(owner)?);
                    continue;
                }
                let symbols = self.nodes[owner].names.get(part)?;
                symbol = select(symbols, if position == parts.len() { kind } else { None })?;
                if !self.accessible(from, &symbol) {
                    return None;
                }
            } else {
                let symbols = self.associated.get(&symbol.id)?.get(part)?;
                symbol = select(symbols, if position == parts.len() { kind } else { None })?;
                if !self.accessible(from, &symbol) {
                    return None;
                }
            }
        }
        if kind.is_some_and(|k| k != symbol.kind) {
            None
        } else {
            Some(symbol)
        }
    }
    fn imports(&mut self) -> Result<()> {
        use std::collections::HashSet;
        let imports: Vec<_> = self
            .nodes
            .iter()
            .enumerate()
            .flat_map(|(module, node)| {
                node.ast
                    .imports
                    .iter()
                    .cloned()
                    .map(move |import| (module, import))
            })
            .collect();
        let mut installed: Vec<HashSet<(String, Kind, Box<str>)>> =
            vec![HashSet::new(); imports.len()];
        let mut origins: HashMap<(usize, String, Kind), Vec<usize>> = HashMap::new();
        loop {
            let mut changed = false;
            let mut unresolved = None;
            for (ordinal, (module, import)) in imports.iter().enumerate() {
                let Some(target) = self.path(*module, &import.path, None) else {
                    unresolved.get_or_insert(import);
                    continue;
                };
                self.check_import(*module, &target, import.span)?;
                let entries = if import.glob {
                    if target.kind != Kind::Module {
                        return Err(error("glob import requires a module", import.span));
                    }
                    {
                        let node = self
                            .nodes
                            .iter()
                            .find(|node| node.path == target.target.as_ref())
                            .expect("resolved module");
                        node.names
                            .iter()
                            .flat_map(|(name, symbols)| {
                                symbols
                                    .iter()
                                    .filter(|symbol| symbol.public)
                                    .map(move |symbol| (name.clone(), symbol.clone()))
                            })
                            .collect()
                    }
                } else {
                    vec![(
                        import
                            .alias
                            .as_deref()
                            .unwrap_or_else(|| import.path.rsplit("::").next().unwrap())
                            .to_owned(),
                        target,
                    )]
                };
                for (name, mut symbol) in entries {
                    self.check_import(*module, &symbol, import.span)?;
                    if !installed[ordinal].insert((
                        name.clone(),
                        symbol.kind,
                        symbol.target.clone(),
                    )) {
                        continue;
                    }
                    if import.public && !symbol.public {
                        return Err(error(
                            "cannot publicly reexport a private declaration",
                            import.span,
                        ));
                    }
                    let key = (*module, name.clone(), symbol.kind);
                    let previous = origins.entry(key).or_default();
                    let existing = self.nodes[*module].names.entry(name.clone()).or_default();
                    if let Some(existing) = existing
                        .iter_mut()
                        .find(|existing| existing.kind == symbol.kind)
                    {
                        if existing.target != symbol.target
                            || (!import.glob
                                && (previous.is_empty()
                                    || previous.iter().any(|previous| !imports[*previous].1.glob)))
                        {
                            return Err(error(
                                format!("ambiguous import `{}`", cyallo_ast::source_name(&name)),
                                import.span,
                            ));
                        }
                        if import.public && !existing.public {
                            existing.public = true;
                            changed = true;
                        }
                    } else {
                        symbol.public = import.public;
                        symbol.owner = *module;
                        existing.push(symbol);
                        changed = true;
                    }
                    previous.push(ordinal);
                }
            }
            if !changed {
                if let Some(import) = unresolved {
                    return Err(error(
                        format!("unresolved or private import {}", import.path),
                        import.span,
                    ));
                }
                return Ok(());
            }
        }
    }
    fn symbol(&self, module: usize, name: &str, kind: Kind, span: Span) -> Result<Box<str>> {
        let symbol = self.path(module, name, Some(kind)).ok_or_else(|| {
            error(
                format!("unresolved name `{}`", cyallo_ast::source_name(name)),
                span,
            )
        })?;
        self.check_import(module, &symbol, span)?;
        Ok(symbol.target)
    }
    fn ty(&self, module: usize, ty: &mut Type, params: &[Box<str>], span: Span) -> Result<()> {
        self.ty_alias(module, ty, params, span, &mut vec![])
    }
    fn ty_alias(
        &self,
        module: usize,
        ty: &mut Type,
        params: &[Box<str>],
        span: Span,
        seen: &mut Vec<Box<str>>,
    ) -> Result<()> {
        match ty {
            Type::Lifetime(_) => {
                return Err(error("lifetime is only valid as a lifetime argument", span));
            }
            Type::Named(n) if params.contains(n) => {}
            Type::Named(n) | Type::TraitObject(n) => {
                let target = self.symbol(module, n, Kind::Type, span)?;
                if let Some((owner, alias)) = self.aliases.get(&target) {
                    if seen.contains(&target) {
                        return Err(cyallo_ast::diagnostic::cyclic_type_alias(span));
                    }
                    if seen.len() >= 256 {
                        return Err(cyallo_ast::diagnostic::type_alias_depth(span));
                    }
                    seen.push(target);
                    let mut alias = alias.clone();
                    cyallo_ast::with_stack(|| self.ty_alias(*owner, &mut alias, &[], span, seen))?;
                    seen.pop();
                    *ty = alias;
                    return Ok(());
                }
                if let Some((_, InternalSemantic::Type(id))) = self.native_target(&target) {
                    *ty = match id {
                        NativeType::Any => Type::Any,
                        NativeType::Symbol => Type::Symbol,
                        NativeType::Context => Type::Context,
                        NativeType::Registration => Type::Registration,
                        NativeType::Storage(
                            kind @ (StorageKind::TypeDescriptor
                            | StorageKind::ViewError
                            | StorageKind::LoadError
                            | StorageKind::ParseError
                            | StorageKind::Layout
                            | StorageKind::Allocation
                            | StorageKind::AllocError
                            | StorageKind::ReceiveError
                            | StorageKind::Clock),
                        ) => Type::Storage(kind, Box::new(Type::Void)),
                        _ => return Err(error("generic type requires element arguments", span)),
                    };
                } else {
                    *n = target;
                }
            }
            Type::Applied(n, arguments) => {
                let target = self.symbol(module, n, Kind::Type, span)?;
                if matches!(
                    self.native_target(&target),
                    Some((
                        _,
                        InternalSemantic::Type(NativeType::Storage(StorageKind::Weak))
                    ))
                ) {
                    let lifetime = if matches!(arguments.first(), Some(Type::Lifetime(_))) {
                        let Type::Lifetime(lifetime) = arguments.remove(0) else {
                            unreachable!()
                        };
                        Some(lifetime)
                    } else {
                        None
                    };
                    if arguments.len() != 1 {
                        return Err(error(
                            "Weak requires a pointee type and an optional allocator lifetime",
                            span,
                        ));
                    }
                    let mut pointee = arguments.remove(0);
                    self.ty_alias(module, &mut pointee, params, span, seen)?;
                    *ty = Type::Storage(
                        StorageKind::Weak,
                        Box::new(Type::Managed {
                            kind: MemoryKind::Rc,
                            locality: Locality::Local,
                            lifetime,
                            pointee: Box::new(pointee),
                        }),
                    );
                    return Ok(());
                }
                for t in arguments.iter_mut() {
                    self.ty_alias(module, t, params, span, seen)?;
                }
                let target = self.symbol(module, n, Kind::Type, span)?;
                if let Some((declaration, InternalSemantic::TypeFamily(_))) =
                    self.native_target(&target)
                {
                    *ty = Type::Dependent(declaration, std::mem::take(arguments));
                } else if let Some((_, InternalSemantic::Type(id))) = self.native_target(&target) {
                    let expected = declarations::type_arity(id);
                    if arguments.len() != expected {
                        return Err(invalid_declaration(
                            DeclarationError::GenericArity {
                                expected,
                                actual: arguments.len(),
                            },
                            span,
                        ));
                    }
                    let inner = Box::new(if expected == 1 {
                        arguments.remove(0)
                    } else {
                        Type::Tuple(std::mem::take(arguments))
                    });
                    *ty = match id {
                        NativeType::ServiceKey => Type::ServiceKey(inner),
                        NativeType::ServiceRef => Type::ServiceRef(inner),
                        NativeType::Storage(StorageKind::Weak) => Type::Storage(
                            StorageKind::Weak,
                            Box::new(Type::Managed {
                                kind: MemoryKind::Rc,
                                locality: Locality::Local,
                                lifetime: None,
                                pointee: inner,
                            }),
                        ),
                        NativeType::Storage(kind) => Type::Storage(kind, inner),
                        _ => return Err(error("type does not accept element arguments", span)),
                    };
                } else {
                    *n = target;
                }
            }
            Type::Array(_, t)
            | Type::SparseArray(t)
            | Type::Borrowed(t)
            | Type::Slice(t)
            | Type::ServiceKey(t)
            | Type::ServiceRef(t)
            | Type::Storage(_, t) => self.ty_alias(module, t, params, span, seen)?,
            Type::Managed {
                locality: Locality::Shared,
                ..
            } => return Err(cyallo_ast::diagnostic::shared_managed_unavailable(span)),
            Type::Reference { pointee, .. } | Type::Managed { pointee, .. } => {
                self.ty_alias(module, pointee, params, span, seen)?
            }
            Type::Tuple(ts) => {
                for t in ts {
                    self.ty_alias(module, t, params, span, seen)?;
                }
            }
            Type::Dependent(_, arguments) => {
                for argument in arguments {
                    self.ty_alias(module, argument, params, span, seen)?;
                }
            }
            Type::Function(_, ps, r) => {
                for (t, _) in ps {
                    self.ty_alias(module, t, params, span, seen)?;
                }
                self.ty_alias(module, r, params, span, seen)?;
            }
            Type::Record(_, fs) | Type::Object(fs) => {
                let mut names = std::collections::HashSet::new();
                for (name, _) in fs.iter() {
                    if !names.insert(name) {
                        return Err(cyallo_ast::diagnostic::duplicate_object_property(span));
                    }
                }
                for (_, t) in fs {
                    self.ty_alias(module, t, params, span, seen)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn local(scopes: &[HashMap<String, Local>], name: &str) -> Option<Local> {
        scopes.iter().rev().find_map(|s| s.get(name).cloned())
    }
    fn fresh(&mut self, name: &str) -> Box<str> {
        self.next_binding += 1;
        let target: Box<str> = format!("$binding{}${name}", self.next_binding).into();
        self.source_names.insert(target.clone(), name.into());
        target
    }
    fn expr(
        &mut self,
        module: usize,
        e: &mut Expr,
        scopes: &mut Vec<HashMap<String, Local>>,
        params: &[Box<str>],
    ) -> Result<()> {
        cyallo_ast::with_stack(|| {
            self.requesting_modules
                .insert(e.span.start, self.nodes[module].id);
            let constructor = match &e.kind {
                ExprKind::Index(base, _) => match &base.kind {
                    ExprKind::Name(n) if Self::local(scopes, n).is_none() => Some(n.as_ref()),
                    _ => None,
                },
                ExprKind::TypedCollection(n, _, _) => Some(n.as_ref()),
                _ => None,
            }
            .and_then(|name| self.path(module, name, Some(Kind::Type)));
            if let Some(symbol) = &constructor {
                self.check_import(module, symbol, e.span)?;
            }
            let constructor = constructor.and_then(|symbol| match symbol.native {
                Some(InternalSemantic::Type(NativeType::Storage(StorageKind::Vec))) => {
                    Some(Intrinsic::VecLiteral)
                }
                Some(InternalSemantic::Type(NativeType::Storage(StorageKind::VecDeque))) => {
                    Some(Intrinsic::DequeLiteral)
                }
                _ => None,
            });
            if let Some(id) = constructor {
                match &mut e.kind {
                    ExprKind::Index(_, args) => {
                        e.kind = ExprKind::IntrinsicCall(id.id(), vec![], std::mem::take(args))
                    }
                    ExprKind::TypedCollection(_, types, args) => {
                        e.kind = ExprKind::IntrinsicCall(
                            id.id(),
                            std::mem::take(types),
                            std::mem::take(args),
                        )
                    }
                    _ => {}
                }
            }
            match &mut e.kind {
                ExprKind::ObjectLiteral(_, fields) => {
                    for (_, value) in fields {
                        self.expr(module, value, scopes, params)?;
                    }
                }
                ExprKind::DeleteProperty(value) => self.expr(module, value, scopes, params)?,
                ExprKind::TypeDescriptor(_) | ExprKind::DeferredCallable(_) => {}
                ExprKind::NativeCoerce(value, thunk) => {
                    self.expr(module, value, scopes, params)?;
                    self.expr(module, thunk, scopes, params)?;
                }
                ExprKind::Yield(value, _) => self.expr(module, value, scopes, params)?,
                ExprKind::ManagedNew(_, locality, allocator, value) => {
                    if *locality == Locality::Shared {
                        return Err(cyallo_ast::diagnostic::shared_managed_unavailable(e.span));
                    }
                    if let Some(allocator) = allocator {
                        self.expr(module, allocator, scopes, params)?;
                    }
                    self.expr(module, value, scopes, params)?;
                }
                ExprKind::TypedCollection(..) => {
                    return Err(error(
                        "type does not define a collection initializer",
                        e.span,
                    ));
                }
                ExprKind::IntrinsicCall(_, types, arguments) => {
                    for t in types {
                        self.ty(module, t, params, e.span)?;
                    }
                    for a in arguments {
                        self.expr(module, a, scopes, params)?;
                    }
                }
                ExprKind::Name(n) => {
                    *n = if let Some(local) = Self::local(scopes, n) {
                        local.target
                    } else if let Some(symbol) = self.path(module, n, Some(Kind::Value)) {
                        self.check_import(module, &symbol, e.span)?;
                        symbol.target
                    } else {
                        e.kind = ExprKind::Invalid(Diagnostic::new(
                            "E106",
                            format!("undeclared identifier `{}`", cyallo_ast::source_name(n)),
                            e.span,
                        ));
                        return Ok(());
                    }
                }
                ExprKind::Call(n, args) | ExprKind::GenericCall(n, _, args) => {
                    let symbol = if Self::local(scopes, n).is_none() {
                        self.path(module, n, Some(Kind::Value))
                    } else {
                        None
                    };
                    if let Some(symbol) = &symbol {
                        self.check_import(module, symbol, e.span)?;
                    }
                    for a in args.iter_mut() {
                        self.expr(module, a, scopes, params)?;
                    }
                    let mut types = match &mut e.kind {
                        ExprKind::GenericCall(_, ts, _) => ts.clone(),
                        _ => vec![],
                    };
                    for t in &mut types {
                        self.ty(module, t, params, e.span)?;
                    }
                    if let Some(id) = symbol.as_ref().and_then(|s| s.intrinsic) {
                        let args = match &mut e.kind {
                            ExprKind::Call(_, a) | ExprKind::GenericCall(_, _, a) => {
                                std::mem::take(a)
                            }
                            _ => unreachable!(),
                        };
                        e.kind = ExprKind::IntrinsicCall(
                            id.id().declared(symbol.as_ref().unwrap().id),
                            types,
                            args,
                        );
                    } else {
                        let name = match &mut e.kind {
                            ExprKind::Call(n, _) | ExprKind::GenericCall(n, _, _) => n,
                            _ => unreachable!(),
                        };
                        *name = if let Some(local) = Self::local(scopes, name) {
                            local.target
                        } else {
                            if let Some(symbol) = symbol {
                                symbol.target
                            } else {
                                e.kind = ExprKind::Invalid(Diagnostic::new(
                                    "E106",
                                    format!(
                                        "undeclared function `{}`",
                                        cyallo_ast::source_name(name)
                                    ),
                                    e.span,
                                ));
                                return Ok(());
                            }
                        };
                        if let ExprKind::GenericCall(_, ts, _) = &mut e.kind {
                            *ts = types
                        }
                    }
                }
                ExprKind::Record(n, fs) | ExprKind::GenericRecord(n, _, fs) => {
                    if let Some(symbol) = self.path(module, n, Some(Kind::Type)) {
                        self.check_import(module, &symbol, e.span)?;
                        if symbol.native.is_some() {
                            return Err(invalid_declaration(
                                DeclarationError::OpaqueConstruction,
                                e.span,
                            ));
                        }
                    }
                    *n = self.symbol(module, n, Kind::Type, e.span)?;
                    for (_, value) in fs {
                        self.expr(module, value, scopes, params)?;
                    }
                    if let ExprKind::GenericRecord(_, ts, _) = &mut e.kind {
                        for t in ts {
                            self.ty(module, t, params, e.span)?;
                        }
                    }
                }
                ExprKind::ArrayLiteral(es) | ExprKind::Tuple(es) => {
                    for value in es {
                        self.expr(module, value, scopes, params)?;
                    }
                }
                ExprKind::Field(base, name) => {
                    self.expr(module, base, scopes, params)?;
                    if let Some(property) = self.path(module, name, Some(Kind::Property)) {
                        self.check_import(module, &property, e.span)?;
                        e.kind =
                            ExprKind::ExtensionField(base.clone(), name.clone(), property.target);
                    }
                }
                ExprKind::Parenthesized(base)
                | ExprKind::Unary(_, base)
                | ExprKind::TupleField(base, _) => self.expr(module, base, scopes, params)?,
                ExprKind::Cast(base, t) => {
                    self.expr(module, base, scopes, params)?;
                    self.ty(module, t, params, e.span)?;
                }
                ExprKind::Binary(_, a, b) => {
                    self.expr(module, a, scopes, params)?;
                    self.expr(module, b, scopes, params)?;
                }
                ExprKind::Index(base, indices) | ExprKind::Invoke(base, indices) => {
                    self.expr(module, base, scopes, params)?;
                    for index in indices {
                        self.expr(module, index, scopes, params)?;
                    }
                }
                ExprKind::Closure(f, refs) => {
                    for capture in refs {
                        let n = &mut capture.name;
                        *n = if let Some(local) = Self::local(scopes, n) {
                            local.target
                        } else {
                            self.symbol(module, n, Kind::Value, e.span)?
                        };
                    }
                    self.function(module, f, scopes, params)?;
                }
                ExprKind::Frozen(_, t) => self.ty(module, t, params, e.span)?,
                ExprKind::Literal(_)
                | ExprKind::ClosureValue(..)
                | ExprKind::ExtensionField(..)
                | ExprKind::Invalid(_) => {}
            }
            Ok(())
        })
    }
    fn block(
        &mut self,
        module: usize,
        body: &mut [Stmt],
        scopes: &mut Vec<HashMap<String, Local>>,
        params: &[Box<str>],
        top: bool,
    ) -> Result<()> {
        cyallo_ast::with_stack(|| {
            scopes.push(HashMap::new());
            for s in body {
                match &mut s.kind {
                    StmtKind::Binding(name, ty, _, mutable, value) => {
                        if let Some(t) = ty {
                            self.ty(module, t, params, s.span)?;
                        }
                        self.expr(module, value, scopes, params)?;
                        let target = if top {
                            self.nodes[module]
                                .bindings
                                .get(&s.span.start)
                                .cloned()
                                .expect("declared binding")
                        } else {
                            self.fresh(name)
                        };
                        let writable = *mutable
                            || matches!(ty, Some(Type::Reference { mutable: true, .. }))
                            || matches!(value.kind, ExprKind::Unary(Operator::BorrowMut, _));
                        scopes.last_mut().unwrap().insert(
                            name.to_string(),
                            Local {
                                target: target.clone(),
                                writable,
                            },
                        );
                        *name = target;
                    }
                    StmtKind::Constant(name, value) => {
                        self.expr(module, value, scopes, params)?;
                        let target = if top {
                            self.symbol(module, name, Kind::Value, s.span)?
                        } else {
                            self.fresh(name)
                        };
                        scopes.last_mut().unwrap().insert(
                            name.to_string(),
                            Local {
                                target: target.clone(),
                                writable: false,
                            },
                        );
                        *name = target;
                    }
                    StmtKind::Assign(target, value) | StmtKind::Compound(target, _, value) => {
                        fn root(e: &Expr) -> Option<&str> {
                            match &e.kind {
                                ExprKind::Name(n) => Some(n),
                                ExprKind::Parenthesized(b) => root(b),
                                _ => None,
                            }
                        }
                        if let Some(name) = root(target)
                            && let Some(local) = Self::local(scopes, name)
                            && !local.writable
                        {
                            return Err(Diagnostic::new(
                                "E107",
                                "cannot modify an immutable binding",
                                target.span,
                            ));
                        }
                        self.expr(module, target, scopes, params)?;
                        self.expr(module, value, scopes, params)?;
                    }
                    StmtKind::Evaluate(e) | StmtKind::Return(e) | StmtKind::Input(e) => {
                        self.expr(module, e, scopes, params)?
                    }
                    StmtKind::Output(es) => {
                        for e in es {
                            self.expr(module, e, scopes, params)?;
                        }
                    }
                    StmtKind::Call(n, es) => {
                        *n = self.symbol(module, n, Kind::Value, s.span)?;
                        for e in es {
                            self.expr(module, e, scopes, params)?;
                        }
                    }
                    StmtKind::If(condition, a, b) => {
                        self.expr(module, condition, scopes, params)?;
                        self.block(module, a, scopes, params, false)?;
                        self.block(module, b, scopes, params, false)?;
                    }
                    StmtKind::While(condition, b) | StmtKind::Repeat(b, condition) => {
                        self.expr(module, condition, scopes, params)?;
                        self.block(module, b, scopes, params, false)?;
                    }
                    StmtKind::ForIn(name, iterable, b) => {
                        self.expr(module, iterable, scopes, params)?;
                        let target = self.fresh(name);
                        scopes.push(HashMap::from([(
                            name.to_string(),
                            Local {
                                target: target.clone(),
                                writable: false,
                            },
                        )]));
                        *name = target;
                        self.block(module, b, scopes, params, false)?;
                        scopes.pop();
                    }
                    StmtKind::For(name, first, last, step, b) => {
                        self.expr(module, first, scopes, params)?;
                        self.expr(module, last, scopes, params)?;
                        if let Some(e) = step {
                            self.expr(module, e, scopes, params)?;
                        }
                        let target = self.fresh(name);
                        scopes.push(HashMap::from([(
                            name.to_string(),
                            Local {
                                target: target.clone(),
                                writable: false,
                            },
                        )]));
                        *name = target;
                        self.block(module, b, scopes, params, false)?;
                        scopes.pop();
                    }
                    StmtKind::Case(..) | StmtKind::Declare(..) => {
                        return Err(error("unsupported modern statement", s.span));
                    }
                    StmtKind::Break | StmtKind::Continue | StmtKind::ReturnVoid => {}
                }
            }
            scopes.pop();
            Ok(())
        })
    }
    fn function(
        &mut self,
        module: usize,
        f: &mut Function,
        outer: &[HashMap<String, Local>],
        inherited_params: &[Box<str>],
    ) -> Result<()> {
        if !f.annotations.is_empty() {
            declarations::bind(
                &f.annotations,
                self.nodes[module].trust,
                DeclarationKind::Member,
            )?;
        }
        let mut scopes = outer.to_vec();
        let mut parameters = HashMap::new();
        let mut type_params = inherited_params.to_vec();
        type_params.extend(f.type_params.iter().cloned());
        for constraint in &mut f.constraints {
            for bound in &mut constraint.bounds {
                self.ty(module, bound, &type_params, f.span)?;
            }
        }
        if f.body_kind == FunctionBodyKind::Declaration
            && self.nodes[module].trust == SourceTrust::Core
        {
            let names: Vec<_> = f.params.iter().map(|param| param.name.clone()).collect();
            cyallo_ast::visit::visit_type(&mut f.result, &mut |ty| {
                if let Type::Named(name) = ty
                    && !type_params.contains(name)
                    && let Some(index) = names.iter().position(|parameter| parameter == name)
                {
                    *ty = Type::ValueParameter(index);
                }
            });
        }
        for p in &mut f.params {
            self.ty(module, &mut p.ty, &type_params, p.span)?;
            if parameters.contains_key(p.name.as_ref()) {
                return Err(error("duplicate parameter", p.span));
            }
            let target = self.fresh(&p.name);
            parameters.insert(
                p.name.to_string(),
                Local {
                    target: target.clone(),
                    writable: p.by_ref || matches!(p.ty, Type::Reference { mutable: true, .. }),
                },
            );
            p.name = target;
        }
        self.ty(module, &mut f.result, &type_params, f.span)?;
        scopes.push(parameters);
        self.block(module, &mut f.body, &mut scopes, &type_params, false)
    }
    fn finish(mut self, entry: usize) -> Result<Ast> {
        self.declarations(entry)?;
        self.imports()?;
        let mut linked = Ast::default();
        for (module, mut declaration) in std::mem::take(&mut self.associated_functions) {
            self.function(module, &mut declaration.function, &[], &[])?;
            if declaration.function.body_kind == FunctionBodyKind::Source
                && declaration.function.annotations.is_empty()
            {
                linked.functions.push(declaration.function);
            } else {
                linked.native.functions.push(declaration);
            }
        }
        let mut constants = Vec::new();
        for index in 0..self.nodes.len() {
            let mut ast = std::mem::take(&mut self.nodes[index].ast);
            for record in &mut ast.records {
                let symbol = self.path(index, &record.name, Some(Kind::Type)).unwrap();
                record.name = symbol.target;
                for (_, t) in &mut record.fields {
                    self.ty(index, t, &record.type_params, record.span)?;
                }
            }
            for f in &mut ast.functions {
                f.name = self.symbol(index, &f.name, Kind::Value, f.span)?;
                self.function(index, f, &[], &[])?;
            }
            for t in &mut ast.traits {
                let symbol = self.path(index, &t.name, Some(Kind::Type)).unwrap();
                if symbol.native == Some(InternalSemantic::AllocatorTrait) {
                    t.origin = TraitOrigin::Allocator;
                }
                t.name = symbol.target;
                for f in &mut t.methods {
                    self.function(index, f, &[], &t.type_params)?;
                }
            }
            for imp in &mut ast.implementations {
                imp.trait_name = self.symbol(index, &imp.trait_name, Kind::Type, imp.span)?;
                self.ty(index, &mut imp.receiver, &[], imp.span)?;
                for f in &mut imp.methods {
                    self.function(index, f, &[], &[])?;
                }
            }
            for property in &mut ast.properties {
                self.ty(index, &mut property.receiver, &[], property.span)?;
                property.getter.name =
                    self.symbol(index, &property.name, Kind::Property, property.span)?;
                self.function(index, &mut property.getter, &[], &[])?;
            }
            for implementation in &mut ast.inherent_implementations {
                let mut receiver = implementation.receiver.clone();
                let owner = match &receiver {
                    Type::Named(name) | Type::Applied(name, _) => self
                        .path(index, name, Some(Kind::Type))
                        .map(|symbol| symbol.id),
                    _ => None,
                }
                .unwrap_or_else(|| self.declaration_id());
                self.ty(
                    index,
                    &mut receiver,
                    &implementation.type_params,
                    implementation.span,
                )?;
                for method in &mut implementation.methods {
                    if !method
                        .function
                        .params
                        .first()
                        .is_some_and(|param| param.receiver)
                    {
                        continue;
                    }
                    let semantic = declarations::bind(
                        &method.function.annotations,
                        self.nodes[index].trust,
                        DeclarationKind::Member,
                    )?;
                    if semantic.is_none() {
                        return Err(invalid_declaration(
                            DeclarationError::InvalidSignature,
                            method.function.span,
                        ));
                    }
                    let mut function = method.function.clone();
                    for param in &mut function.params {
                        cyallo_ast::visit::visit_type(&mut param.ty, &mut |ty| {
                            if *ty == Type::SelfType {
                                *ty = receiver.clone();
                            }
                        });
                    }
                    cyallo_ast::visit::visit_type(&mut function.result, &mut |ty| {
                        if *ty == Type::SelfType {
                            *ty = receiver.clone();
                        }
                    });
                    self.function(index, &mut function, &[], &implementation.type_params)?;
                    linked.native.methods.push(NativeMethodDeclaration {
                        id: self.declaration_id(),
                        module: Some(self.nodes[index].id),
                        owner,
                        owner_parameters: implementation.type_params.clone(),
                        receiver: receiver.clone(),
                        function,
                        public: method.public,
                    });
                }
            }
            ast.functions.retain(|function| {
                if function.annotations.is_empty() {
                    true
                } else {
                    let id = self.native_target(&function.name).unwrap().0;
                    linked.native.functions.push(NativeFunctionDeclaration {
                        id,
                        module: Some(self.nodes[index].id),
                        function: function.clone(),
                    });
                    false
                }
            });
            ast.records
                .retain(|record| match self.native_target(&record.name) {
                    Some((id, InternalSemantic::Type(_))) => {
                        linked.native.types.push(NativeTypeDeclaration {
                            id,
                            name: record.name.clone(),
                            parameters: record.type_params.clone(),
                            annotation: record.annotations[0].clone(),
                        });
                        false
                    }
                    Some((id, InternalSemantic::TypeFamily(_))) => {
                        linked.native.families.push((id, record.clone()));
                        false
                    }
                    _ => true,
                });
            ast.traits.retain(|t| match self.native_target(&t.name) {
                Some((id, InternalSemantic::Trait(_))) => {
                    linked.native.traits.push((id, t.clone()));
                    false
                }
                Some((id, InternalSemantic::AllocatorTrait)) => {
                    linked.native.traits.push((id, t.clone()));
                    true
                }
                _ => true,
            });
            if index != entry
                && ast.body.iter().any(|s| {
                    !matches!(
                        s.kind,
                        StmtKind::Binding(_, _, true, _, _) | StmtKind::Constant(..)
                    )
                })
            {
                return Err(error(
                    "module initialization requires constants or an explicit procedure",
                    ast.body[0].span,
                ));
            }
            self.block(index, &mut ast.body, &mut Vec::new(), &[], true)?;
            if index == entry {
                linked.body.append(&mut ast.body);
            } else {
                constants.append(&mut ast.body);
            }
            linked.functions.append(&mut ast.functions);
            linked.records.append(&mut ast.records);
            linked.traits.append(&mut ast.traits);
            linked.implementations.append(&mut ast.implementations);
            linked.properties.append(&mut ast.properties);
        }
        declarations::validate_source_ast(&linked)?;
        declarations::validate_native_declarations(&linked.native)?;
        // Topologically order imported constants without executing module code.
        let names: HashMap<_, _> = constants
            .iter()
            .enumerate()
            .filter_map(|(i, s)| match &s.kind {
                StmtKind::Binding(n, ..) | StmtKind::Constant(n, _) => Some((n.clone(), i)),
                _ => None,
            })
            .collect();
        let mut dependencies = vec![Vec::new(); constants.len()];
        for (i, s) in constants.iter_mut().enumerate() {
            cyallo_ast::visit::visit_block(
                std::slice::from_mut(s),
                &mut |e| {
                    if let ExprKind::Name(n) = &e.kind
                        && let Some(j) = names.get(n)
                    {
                        dependencies[i].push(*j);
                    }
                    Ok(())
                },
                &mut |_, _| Ok(()),
            )?;
        }
        fn order(
            i: usize,
            deps: &[Vec<usize>],
            marks: &mut [u8],
            out: &mut Vec<usize>,
            body: &[Stmt],
        ) -> Result<()> {
            if marks[i] == 2 {
                return Ok(());
            }
            if marks[i] == 1 {
                return Err(error("cyclic constant initialization", body[i].span));
            }
            marks[i] = 1;
            for j in &deps[i] {
                order(*j, deps, marks, out, body)?;
            }
            marks[i] = 2;
            out.push(i);
            Ok(())
        }
        let mut marks = vec![0; constants.len()];
        let mut sorted = Vec::new();
        for i in 0..constants.len() {
            order(i, &dependencies, &mut marks, &mut sorted, &constants)?;
        }
        let mut body: Vec<_> = sorted.into_iter().map(|i| constants[i].clone()).collect();
        body.append(&mut linked.body);
        linked.body = body;
        linked.source_names = std::mem::take(&mut self.source_names);
        linked.resolved_modules = self
            .nodes
            .iter()
            .map(|node| {
                let mut values: Vec<_> = node
                    .names
                    .iter()
                    .flat_map(|(name, symbols)| {
                        symbols
                            .iter()
                            .filter(|symbol| symbol.kind != Kind::Property && symbol.public)
                            .map(|symbol| ResolvedExport {
                                kind: match symbol.kind {
                                    Kind::Value => ExportKind::Value,
                                    Kind::Module => ExportKind::Module,
                                    Kind::Type => ExportKind::Type,
                                    Kind::Property => unreachable!(),
                                },
                                name: name.clone().into(),
                                target: symbol.target.clone(),
                            })
                    })
                    .collect();
                values.sort_by(|left, right| left.name.cmp(&right.name));
                ResolvedModuleExports {
                    path: node.path.clone().into(),
                    values,
                }
            })
            .collect();
        if let Some(permissions) = &self.permissions {
            let graph = self.module_graph();
            linked.native.import_policy = Some(NativeImportPolicy {
                modules: graph
                    .iter()
                    .map(|module| NativeModuleIdentity {
                        trust: module.trust,
                        id: module.id,
                        path: module.path.clone(),
                    })
                    .collect(),
                access: graph
                    .iter()
                    .map(|from| NativeModuleAccess {
                        referrer: from.id,
                        allowed: graph
                            .iter()
                            .filter(|target| permissions.check(from.id, target.id).is_ok())
                            .map(|module| module.id)
                            .collect(),
                    })
                    .collect(),
                requesters: self.requesting_modules,
            });
        }
        Ok(linked)
    }
}
pub fn link(asts: Vec<Ast>, paths: &[String], entry: usize) -> Result<Ast> {
    link_with_policy(
        asts,
        paths,
        entry,
        &crate::import_policy::ImportPolicy::default(),
    )
}
pub fn link_with_policy(
    asts: Vec<Ast>,
    paths: &[String],
    entry: usize,
    policy: &crate::import_policy::ImportPolicy,
) -> Result<Ast> {
    link_with_core(
        asts,
        paths,
        entry,
        policy,
        &crate::core_package::CorePackage::embedded(),
        0,
    )
}
pub fn link_with_core(
    asts: Vec<Ast>,
    paths: &[String],
    entry: usize,
    policy: &crate::import_policy::ImportPolicy,
    package: &crate::core_package::CorePackage<'_>,
    core_offset: u32,
) -> Result<Ast> {
    let mut files: Vec<_> = asts.into_iter().map(Some).collect();
    let root = files[entry]
        .take()
        .ok_or_else(|| error("missing entry module", Span::default()))?;
    let dir = Path::new(&paths[entry]).parent().unwrap_or(Path::new(""));
    let mut resolver = Resolver {
        aliases: HashMap::new(),
        source_names: HashMap::new(),
        nodes: Vec::new(),
        next_binding: 0,
        next_declaration: 0,
        prelude: None,
        packages: HashMap::new(),
        associated: HashMap::new(),
        associated_functions: vec![],
        permissions: None,
        requesting_modules: HashMap::new(),
    };
    // Only bytes furnished by the sealed core source provider receive Core
    // provenance. User-controlled filenames and module labels confer no trust.
    let parsed = package.parse(core_offset)?;
    let core_paths = parsed.paths;
    let mut core_files: Vec<_> = parsed.asts.into_iter().map(Some).collect();
    let core_root = parsed.entry;
    let core_ast = core_files[core_root].take().unwrap();
    let core_path = Path::new(package.root());
    let core_name = core_path
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let core = resolver.add(
        core_name.clone(),
        core_ast,
        &mut core_files,
        &core_paths,
        &core_path.with_extension(""),
        Placement {
            trust: SourceTrust::Core,
            parent: None,
            boundary: ModuleBoundary::Package,
        },
    )?;
    resolver.packages.insert(core_name, core);
    // Allocate core identities before user declarations, so appending REPL
    // definitions cannot change the identity of a saved native function item.
    resolver.declarations(0)?;
    let user = resolver.add(
        "crate".into(),
        root,
        &mut files,
        paths,
        dir,
        Placement {
            trust: SourceTrust::User,
            parent: None,
            boundary: ModuleBoundary::Package,
        },
    )?;
    resolver.packages.insert("crate".into(), user);
    resolver.permissions =
        Some(policy.resolve(&resolver.module_graph()).map_err(|error| {
            crate::import_policy::import_policy_diagnostic(error, Span::default())
        })?);
    resolver.finish(user)
}
pub fn single(ast: Ast) -> Result<Ast> {
    link(vec![ast], &["main.ya".into()], 0)
}
