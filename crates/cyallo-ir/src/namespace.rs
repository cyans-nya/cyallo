//! Lexical, case-sensitive resolution shared by single-source and module builds.
mod patterns;
use crate::declarations::{
    self, DeclarationError, DeclarationKind, SourceTrust, invalid_declaration,
};
use crate::internal::{InternalSemantic, NativeType};
use crate::intrinsic::Intrinsic;
use crate::module_graph::{BindingKind, ModuleBinding, ModuleId, ResolvedModule, ResolvedProgram};
use cyallo_ast::*;
use std::collections::{HashMap, HashSet};
use std::path::Path;
type Result<T> = std::result::Result<T, Diagnostic>;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind {
    Module,
    Type,
    Value,
    Property,
    World,
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
/// Where a module's contents come from, which decides how files beside it
/// become child modules.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ModuleSource {
    /// A source file (the entry, `x.ya` or `x/mod.ya`). It gates its children:
    /// an implicit child file is private unless this module re-exports it.
    File,
    /// A directory with no `mod.ya`: an open namespace whose children are public.
    Namespace,
    /// An inline `mod x { ... }`. Files are never scanned beside it.
    Inline,
}
#[derive(Clone, Copy)]
struct Placement {
    trust: SourceTrust,
    parent: Option<usize>,
    boundary: ModuleBoundary,
    source: ModuleSource,
}
struct Node {
    interface: bool,
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
    /// Indexed routine bodies not yet bound to their linked routine, by archived span.
    indexed: HashMap<Span, crate::analysis::IndexedRoutineBody>,
    /// Indexed bodies bound by the linker: MIR plus resolved call targets.
    bound: HashMap<Box<str>, crate::analysis::IndexedRoutineBody>,
    /// Linked symbols of plain `i64` functions a migrated body may call.
    /// Parameter count and whether the callee returns nothing.
    plain: HashMap<Box<str>, (usize, bool)>,
    /// Resolved field types of each record declared without type parameters, by the
    /// record's linked name.
    records: HashMap<Box<str>, Vec<(Box<str>, Type)>>,
    /// The variants of each enum without type parameters, by the enum's linked name, with
    /// their resolved payload types.
    enums: HashMap<Box<str>, Vec<(Box<str>, Option<Type>)>>,
    /// The standard traits a generic routine may be bounded by, by linked name.
    bound_traits: HashMap<Box<str>, crate::generics::Bound>,
    /// The traits the program declares (without type parameters), by linked name.
    user_traits: std::collections::HashSet<Box<str>>,
    /// Enums with type parameters, by linked name: the parameters and the variants'
    /// payload types, which mention them.
    generic_enums: HashMap<Box<str>, (Vec<Box<str>>, Vec<Option<Type>>)>,
    /// The enum each variant belongs to, and whether it carries a payload.
    variants: HashMap<DeclarationId, (Box<str>, bool)>,
    /// Source-file mode: the entry runs `proc main()` and no top level may hold statements.
    entry_mode: crate::EntryMode,
    aliases: HashMap<Box<str>, (usize, Type)>,
    source_names: std::collections::BTreeMap<Box<str>, Box<str>>,
    nodes: Vec<Node>,
    variant_ids: std::collections::HashSet<DeclarationId>,
    unit_variants: std::collections::HashSet<DeclarationId>,
    next_binding: usize,
    next_declaration: u32,
    prelude: Option<usize>,
    packages: HashMap<String, usize>,
    associated: HashMap<DeclarationId, HashMap<String, Vec<Symbol>>>,
    associated_functions: Vec<(usize, NativeFunctionDeclaration)>,
    permissions: Option<crate::import_policy::ResolvedPolicy>,
    requesting_modules: std::collections::BTreeMap<u32, DeclarationId>,
    opaque: std::collections::HashSet<DeclarationId>,
    /// Declared type-parameter count of each provider-bound opaque host type.
    opaque_arity: HashMap<DeclarationId, usize>,
    bound_opaque: HashMap<DeclarationId, Span>,
    implemented_interfaces: Vec<usize>,
    worlds: Vec<crate::module_graph::ResolvedWorld>,
    associated_owners: HashMap<DeclarationId, Box<str>>,
    bindings: crate::bindings::NativeBindings,
    applied_bindings: std::collections::BTreeSet<Box<str>>,
    interface_methods: Vec<InterfaceMethod>,
}
/// A method declared inside an `interface mod`: contract only.
struct InterfaceMethod {
    module: usize,
    id: DeclarationId,
    owner: String,
    public: bool,
    function: Function,
}
fn error(message: impl Into<String>, span: Span) -> Diagnostic {
    Diagnostic::new("E140", message, span)
}
impl Resolver {
    /// Files are modules: each unclaimed source below `dir` that no `mod` line
    /// declared becomes a child. `x.ya` and `x/mod.ya` name module `x`; a
    /// directory with no `mod.ya` is an empty open namespace. Children of a
    /// file module are private (its gate); children of a namespace are public.
    fn implicit_modules(
        declared: &[ModuleDeclaration],
        files: &[Option<Ast>],
        paths: &[String],
        dir: &Path,
        gated: bool,
    ) -> Result<(Vec<ModuleDeclaration>, std::collections::HashSet<Box<str>>)> {
        // An inline `mod b { .. }` and a file for `b` would define it twice.
        for declaration in declared.iter().filter(|d| d.body.is_some()) {
            let name = declaration.name.as_ref();
            let owns_file = [
                dir.join(format!("{name}.ya")),
                dir.join(name).join("mod.ya"),
            ]
            .iter()
            .any(|candidate| {
                paths
                    .iter()
                    .enumerate()
                    .any(|(index, path)| files[index].is_some() && Path::new(path) == candidate)
            });
            if owns_file {
                return Err(error(
                    format!("module `{name}` is declared inline and also defined by a source file"),
                    declaration.span,
                ));
            }
        }
        let mut children: std::collections::BTreeSet<String> = Default::default();
        for (index, path) in paths.iter().enumerate() {
            if files[index].is_none() {
                continue;
            }
            let Ok(relative) = Path::new(path).strip_prefix(dir) else {
                continue;
            };
            let mut parts = relative.components();
            let (Some(first), rest) = (parts.next(), parts.as_path()) else {
                continue;
            };
            let first = first.as_os_str().to_string_lossy();
            let name = if rest.as_os_str().is_empty() {
                match first.strip_suffix(".ya") {
                    Some("mod") | None => continue,
                    Some(name) => name,
                }
            } else {
                first.as_ref()
            };
            if !declared.iter().any(|d| d.name.as_ref() == name) {
                children.insert(name.to_owned());
            }
        }
        for name in &children {
            Self::reject_two_layouts(name, paths, dir, Span::default())?;
        }
        let mut namespaces = std::collections::HashSet::new();
        let modules = children
            .into_iter()
            .map(|name| {
                let child_dir = dir.join(&name);
                let file_backed = paths.iter().enumerate().any(|(index, path)| {
                    files[index].is_some()
                        && (Path::new(path) == dir.join(format!("{name}.ya"))
                            || Path::new(path) == child_dir.join("mod.ya"))
                });
                if !file_backed {
                    namespaces.insert(Box::<str>::from(name.as_str()));
                }
                ModuleDeclaration {
                    interface: false,
                    boundary: ModuleBoundary::Lexical,
                    annotations: Vec::new(),
                    name: name.into(),
                    public: !gated,
                    // A directory without `mod.ya` is a namespace with no items.
                    body: (!file_backed).then(|| Box::new(Ast::default())),
                    span: Span::default(),
                }
            })
            .collect();
        Ok((modules, namespaces))
    }
    /// `x.ya` beside a directory `x/` of sources is ambiguous: a module is either
    /// one file, or a directory with `x/mod.ya`.
    fn reject_two_layouts(name: &str, paths: &[String], dir: &Path, span: Span) -> Result<()> {
        let file = dir.join(format!("{name}.ya"));
        let child_dir = dir.join(name);
        let has_file = paths.iter().any(|path| Path::new(path) == file);
        let has_directory = paths.iter().any(|path| {
            let path = Path::new(path);
            path.starts_with(&child_dir) && path.extension().is_some_and(|e| e == "ya")
        });
        if has_file && has_directory {
            return Err(error(
                format!(
                    "module `{name}` is defined by both `{name}.ya` and the directory `{name}/`; \
                     use `{name}/mod.ya`"
                ),
                span,
            ));
        }
        Ok(())
    }
    /// The fields of every record without type parameters, resolved in its own module.
    fn record_table(&self) -> HashMap<Box<str>, Vec<(Box<str>, Type)>> {
        let mut table = HashMap::new();
        for (index, node) in self.nodes.iter().enumerate() {
            for record in &node.ast.records {
                if !record.type_params.is_empty() {
                    continue;
                }
                let Some(symbol) = self.path(index, &record.name, Some(Kind::Type)) else {
                    continue;
                };
                let fields: Option<Vec<_>> = record
                    .fields
                    .iter()
                    .map(|(name, ty)| Some((name.clone(), self.resolved(index, ty, record.span)?)))
                    .collect();
                if let Some(fields) = fields {
                    table.insert(symbol.target, fields);
                }
            }
        }
        table
    }

    /// The variants of every enum without type parameters, resolved in its own module.
    fn enum_table(&self) -> HashMap<Box<str>, Vec<(Box<str>, Option<Type>)>> {
        let mut table = HashMap::new();
        for (index, node) in self.nodes.iter().enumerate() {
            for declaration in &node.ast.enums {
                if !declaration.type_params.is_empty() || declaration.open {
                    continue;
                }
                let Some(symbol) = self.path(index, &declaration.name, Some(Kind::Type)) else {
                    continue;
                };
                let variants: Option<Vec<_>> = declaration
                    .variants
                    .iter()
                    .map(|variant| {
                        let payload = match &variant.payload {
                            Some(ty) => Some(self.resolved(index, ty, variant.span)?),
                            None => None,
                        };
                        Some((variant.name.clone(), payload))
                    })
                    .collect();
                if let Some(variants) = variants {
                    table.insert(symbol.target, variants);
                }
            }
        }
        table
    }

    /// The marker traits that bound a type parameter, found in the standard modules.
    fn bound_trait_table(&self) -> HashMap<Box<str>, crate::generics::Bound> {
        use crate::generics::Bound;
        let mut table = HashMap::new();
        for (module, name, bound) in [
            ("std::num", "Numeric", Bound::Numeric),
            ("std::num", "Integer", Bound::Integer),
            ("std::num", "Floating", Bound::Floating),
            ("std::ops", "Add", Bound::Add),
            ("std::ops", "Sub", Bound::Sub),
            ("std::ops", "Mul", Bound::Mul),
            ("std::ops", "Div", Bound::Div),
            ("std::ops", "Rem", Bound::Rem),
            ("std::ops", "Neg", Bound::Neg),
            ("std::cmp", "Eq", Bound::Eq),
            ("std::cmp", "Ord", Bound::Ord),
            ("std::mem", "Copy", Bound::Copy),
            ("std::fmt", "Format", Bound::Format),
            ("std::fmt", "Debug", Bound::Debug),
        ] {
            if let Some(index) = self.nodes.iter().position(|node| node.path == module)
                && let Some(symbol) = self.path(index, name, Some(Kind::Type))
            {
                table.insert(symbol.target, bound);
            }
        }
        table
    }

    /// The traits declared in source, which a generic routine may use as a bound.
    fn user_trait_table(&self) -> std::collections::HashSet<Box<str>> {
        let mut table = std::collections::HashSet::new();
        for (index, node) in self.nodes.iter().enumerate() {
            for declaration in &node.ast.traits {
                // A trait with native meaning (`Cloneable`, `Collection`) is decided by the
                // native contract, not by an `impl`, so it is not one a program declares.
                if declaration.type_params.is_empty()
                    && declaration.origin == TraitOrigin::Source
                    && let Some(symbol) = self.path(index, &declaration.name, Some(Kind::Type))
                    && symbol.native.is_none()
                {
                    table.insert(symbol.target);
                }
            }
        }
        table
    }

    /// What each type parameter of a routine is bounded by, with the program's traits it
    /// names, or `None` when a bound is not one the pipeline gives meaning to.
    fn declared_bounds(&self, module: usize, f: &Function) -> Option<DeclaredBounds> {
        let mut declared = Vec::new();
        let mut traits: Vec<Box<str>> = Vec::new();
        for constraint in &f.constraints {
            if !f.type_params.contains(&constraint.parameter) {
                return None;
            }
            let mut bounds = Vec::new();
            for bound in &constraint.bounds {
                let Type::Named(name) = bound else {
                    return None;
                };
                let symbol = self.path(module, name, Some(Kind::Type))?;
                if let Some(bound) = self.bound_traits.get(&symbol.target) {
                    bounds.push(*bound);
                } else if self.user_traits.contains(&symbol.target) {
                    let index = match traits.iter().position(|t| *t == symbol.target) {
                        Some(index) => index,
                        None => {
                            traits.push(symbol.target.clone());
                            traits.len() - 1
                        }
                    };
                    bounds.push(crate::generics::Bound::User(index as u32));
                } else {
                    return None;
                }
            }
            declared.push((constraint.parameter.clone(), bounds));
        }
        Some((declared, traits))
    }

    /// The payload types of every enum with type parameters, resolved in its own module with
    /// the parameters left as names.
    fn generic_enum_table(&self) -> HashMap<Box<str>, (Vec<Box<str>>, Vec<Option<Type>>)> {
        let mut table = HashMap::new();
        for (index, node) in self.nodes.iter().enumerate() {
            for declaration in &node.ast.enums {
                if declaration.type_params.is_empty() || declaration.open {
                    continue;
                }
                let Some(symbol) = self.path(index, &declaration.name, Some(Kind::Type)) else {
                    continue;
                };
                let payloads: Option<Vec<_>> = declaration
                    .variants
                    .iter()
                    .map(|variant| match &variant.payload {
                        Some(ty) => {
                            let mut ty = ty.clone();
                            self.ty(index, &mut ty, &declaration.type_params, variant.span)
                                .ok()?;
                            Some(Some(ty))
                        }
                        None => Some(None),
                    })
                    .collect();
                if let Some(payloads) = payloads {
                    table.insert(symbol.target, (declaration.type_params.to_vec(), payloads));
                }
            }
        }
        table
    }

    /// Whether every instance of a generic enum with scalar type arguments is a type the
    /// pipeline lowers, which decides whether its constructors may be built in place.
    fn generic_enum_supported(&self, owner: &str) -> bool {
        let Some((params, payloads)) = self.generic_enums.get(owner) else {
            return false;
        };
        let probe: Vec<Type> = params.iter().map(|_| Type::Integer).collect();
        self.applied_supported(params, payloads, &probe, 0)
    }

    fn applied_supported(
        &self,
        params: &[Box<str>],
        payloads: &[Option<Type>],
        arguments: &[Type],
        depth: usize,
    ) -> bool {
        params.len() == arguments.len()
            && arguments.iter().all(|argument| {
                !matches!(argument, Type::Reference { .. })
                    && self.pipeline_type(argument, depth + 1)
            })
            && payloads.iter().flatten().all(|payload| {
                let mut payload = payload.clone();
                substitute_named(&mut payload, params, arguments);
                !matches!(payload, Type::Reference { .. })
                    && self.pipeline_type(&payload, depth + 1)
            })
    }

    /// A type as the module graph resolves it, or `None` if it does not resolve.
    fn resolved(&self, module: usize, ty: &Type, span: Span) -> Option<Type> {
        self.resolved_with(module, ty, span, &[])
    }

    /// As `resolved`, with the names of a routine's type parameters left as they are.
    fn resolved_with(
        &self,
        module: usize,
        ty: &Type,
        span: Span,
        params: &[Box<str>],
    ) -> Option<Type> {
        let mut resolved = ty.clone();
        self.ty(module, &mut resolved, params, span).ok()?;
        Some(resolved)
    }

    /// Whether the routine pipeline lowers values of this type: scalars, strings, records
    /// whose fields are all such types, and references to one of those.
    fn pipeline_type(&self, ty: &Type, depth: usize) -> bool {
        match ty {
            Type::Integer | Type::Boolean | Type::Real | Type::Numeric(_) | Type::String => true,
            Type::Reference {
                lifetime: None,
                pointee,
                ..
            } => match &**pointee {
                // A slice of scalars, indexed in place.
                Type::Slice(element) => matches!(
                    **element,
                    Type::Integer | Type::Boolean | Type::Real | Type::Numeric(_)
                ),
                Type::Reference { .. } => false,
                pointee => self.pipeline_type(pointee, depth),
            },
            Type::Applied(target, arguments) if depth < 8 => self
                .generic_enums
                .get(target)
                .is_some_and(|(params, payloads)| {
                    self.applied_supported(params, payloads, arguments, depth)
                }),
            Type::Named(target) if depth < 8 => {
                self.records.get(target).is_some_and(|fields| {
                    fields.iter().all(|(_, field)| {
                        !matches!(field, Type::Reference { .. })
                            && self.pipeline_type(field, depth + 1)
                    })
                }) || self.enums.get(target).is_some_and(|variants| {
                    variants.iter().all(|(_, payload)| {
                        payload.as_ref().is_none_or(|payload| {
                            !matches!(payload, Type::Reference { .. })
                                && self.pipeline_type(payload, depth + 1)
                        })
                    })
                })
            }
            _ => false,
        }
    }

    /// Whether a routine's parameters and result are all types the pipeline lowers, with
    /// every parameter passed by value and no reference as a result.
    fn pipeline_signature(&self, module: usize, f: &Function) -> bool {
        // A type parameter stands for a scalar here: each instance is checked when made.
        let probe: Vec<Type> = f.type_params.iter().map(|_| Type::Integer).collect();
        let resolved = |ty: &Type, span: Span| {
            let mut ty = self.resolved_with(module, ty, span, &f.type_params)?;
            substitute_named(&mut ty, &f.type_params, &probe);
            Some(ty)
        };
        let parameters = f.params.iter().all(|parameter| {
            parameter.arity == ParameterArity::One
                && parameter.passing == ParameterPassing::Value
                && !parameter.by_ref
                && !parameter.receiver
                && resolved(&parameter.ty, parameter.span)
                    .is_some_and(|ty| self.pipeline_type(&ty, 0))
        });
        let result = match f.kind {
            RoutineKind::Procedure if f.result == Type::Void => true,
            _ => resolved(&f.result, f.span).is_some_and(|ty| {
                !matches!(ty, Type::Reference { .. }) && self.pipeline_type(&ty, 0)
            }),
        };
        parameters && result
    }

    /// Linked symbols of source functions with a signature the pipeline lowers.
    fn plain_functions(&self) -> HashMap<Box<str>, (usize, bool)> {
        let mut plain = HashMap::new();
        for (index, node) in self.nodes.iter().enumerate() {
            for f in &node.ast.functions {
                if f.origin == RoutineOrigin::Source
                    && f.body_kind == FunctionBodyKind::Source
                    && !f.generator
                    && f.type_params.is_empty()
                    && f.annotations.is_empty()
                    && self.pipeline_signature(index, f)
                    && let Ok(target) = self.symbol(index, &f.name, Kind::Value, f.span)
                {
                    plain.insert(target, (f.params.len(), f.result == Type::Void));
                }
            }
        }
        plain
    }

    /// Resolve an indexed body's call sites against the module namespace, with no
    /// owned tree. `Err(body)` gives the body back when a site is not a plain call
    /// (a local, an intrinsic, a unit variant, an unresolved name or another
    /// contract); the caller then links the body as a tree, and reports its errors.
    fn bind_indexed(
        &self,
        module: usize,
        function: &Function,
        body: crate::analysis::IndexedRoutineBody,
    ) -> Result<
        std::result::Result<
            crate::analysis::IndexedRoutineBody,
            crate::analysis::IndexedRoutineBody,
        >,
    > {
        // The routine's own signature must be one the pipeline lowers.
        if !self.pipeline_signature(module, function) {
            return Ok(Err(body));
        }
        if !function.constraints.is_empty() && self.declared_bounds(module, function).is_none() {
            return Ok(Err(body));
        }
        let sites: Vec<_> = body
            .reference_sites()
            .map(|site| {
                (
                    Box::<str>::from(site.path),
                    site.arguments,
                    site.discard,
                    site.record,
                    site.name_use,
                    site.contextual,
                    site.span,
                )
            })
            .collect();
        let mut targets = Vec::with_capacity(sites.len());
        for (path, arguments, discard, record, name_use, contextual, span) in sites {
            if record {
                // A record literal names a record type whose fields the pipeline lowers.
                let Some(symbol) = self.path(module, &path, Some(Kind::Type)) else {
                    return Ok(Err(body));
                };
                self.check_import(module, &symbol, span)?;
                if !self.pipeline_type(&Type::Named(symbol.target.clone()), 0) {
                    return Ok(Err(body));
                }
                targets.push(symbol.target);
                continue;
            }
            if function.params.iter().any(|param| param.name == path) {
                return Ok(Err(body));
            }
            if name_use {
                // A name that is not a local: acceptable only as a unit variant.
                let Some(symbol) = self.path(module, &path, Some(Kind::Value)) else {
                    return Ok(Err(body));
                };
                self.check_import(module, &symbol, span)?;
                let supported = self
                    .variants
                    .get(&symbol.id)
                    .is_some_and(|(owner, payload)| {
                        !payload
                            && (self.pipeline_type(&Type::Named(owner.clone()), 0)
                                || (contextual && self.generic_enum_supported(owner)))
                    });
                if !supported {
                    return Ok(Err(body));
                }
                targets.push(symbol.target);
                continue;
            }
            let Some(symbol) = self.path(module, &path, Some(Kind::Value)) else {
                return Ok(Err(body));
            };
            self.check_import(module, &symbol, span)?;
            // A constructor of a supported enum's variant with a payload is a plain callee.
            let constructor = self
                .variants
                .get(&symbol.id)
                .is_some_and(|(owner, payload)| {
                    *payload
                        && arguments == 1
                        && !discard
                        && (self.pipeline_type(&Type::Named(owner.clone()), 0)
                            || (contextual && self.generic_enum_supported(owner)))
                });
            if symbol.intrinsic == Some(crate::intrinsic::Intrinsic::Print) && discard {
                // `print` is built in: the pipeline lowers it to an output, with no callee.
                targets.push("$print".into());
                continue;
            }
            if let Some(id) = symbol.intrinsic
                && crate::builtins::pipeline_utility(id)
            {
                // A utility is typed from its operands and emitted as one operation.
                targets.push(format!("$builtin:{}", id as u16).into());
                continue;
            }
            if symbol.intrinsic.is_some()
                || self.unit_variants.contains(&symbol.id)
                || !(constructor
                    || self
                        .plain
                        .get(&symbol.target)
                        .is_some_and(|&(parameters, void)| {
                            parameters == arguments && (discard || !void)
                        }))
            {
                return Ok(Err(body));
            }
            targets.push(symbol.target);
        }
        Ok(Ok(if targets.is_empty() {
            body
        } else {
            body.with_targets(targets)
        }))
    }
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
        if trust == SourceTrust::Core {
            self.apply_bindings(&path, &mut ast)?;
        }
        let index = self.nodes.len();
        let id = self.declaration_id();
        let mut declarations = std::mem::take(&mut ast.modules);
        let mut namespaces = std::collections::HashSet::new();
        if trust != SourceTrust::Core && placement.source != ModuleSource::Inline {
            let (implicit, empty) = Self::implicit_modules(
                &declarations,
                files,
                paths,
                dir,
                placement.source == ModuleSource::File,
            )?;
            declarations.extend(implicit);
            namespaces = empty;
        }
        let package = if placement.boundary == ModuleBoundary::Package {
            index
        } else {
            placement
                .parent
                .map_or(index, |parent| self.nodes[parent].package)
        };
        self.nodes.push(Node {
            interface: false,
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
            let mut source = ModuleSource::Inline;
            let body = if let Some(body) = declaration.body {
                if namespaces.contains(&declaration.name) {
                    source = ModuleSource::Namespace;
                }
                *body
            } else {
                source = ModuleSource::File;
                if trust != SourceTrust::Core {
                    Self::reject_two_layouts(&declaration.name, paths, dir, declaration.span)?;
                }
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
                    source,
                },
            )?;
            if semantic == Some(InternalSemantic::Prelude) && self.prelude.replace(child).is_some()
            {
                return Err(invalid_declaration(
                    DeclarationError::DuplicateCanonical,
                    declaration.span,
                ));
            }
            self.nodes[child].interface = declaration.interface;
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
    /// Give each declaration its provider binding as the private annotation the rest
    /// of the pipeline reads. Only the provider's own source is touched.
    fn apply_bindings(&mut self, module: &str, ast: &mut Ast) -> Result<()> {
        if self.bindings.is_empty() {
            return Ok(());
        }
        fn attach(
            this: &mut Resolver,
            path: String,
            annotations: &mut Vec<DeclarationAnnotation>,
            span: Span,
        ) -> Result<()> {
            let Some(key) = this.bindings.get(&path) else {
                return Ok(());
            };
            if annotations
                .iter()
                .any(|a| a.name.as_ref() == "cyallo_internal")
            {
                return Err(invalid_declaration(
                    DeclarationError::BindingConflict(path.into()),
                    span,
                ));
            }
            annotations.push(DeclarationAnnotation {
                name: "cyallo_internal".into(),
                value: key.into(),
                span,
            });
            this.applied_bindings.insert(path.into());
            Ok(())
        }
        for function in &mut ast.functions {
            let path = format!("{module}::{}", function.name);
            attach(self, path, &mut function.annotations, function.span)?;
        }
        for record in &mut ast.records {
            let path = format!("{module}::{}", record.name);
            attach(self, path, &mut record.annotations, record.span)?;
        }
        for declaration in &mut ast.enums {
            let path = format!("{module}::{}", declaration.name);
            attach(
                self,
                path.clone(),
                &mut declaration.annotations,
                declaration.span,
            )?;
            for variant in &mut declaration.variants {
                let path = format!("{path}::{}", variant.name);
                attach(self, path, &mut variant.annotations, variant.span)?;
            }
        }
        for declaration in &mut ast.traits {
            let path = format!("{module}::{}", declaration.name);
            attach(self, path, &mut declaration.annotations, declaration.span)?;
        }
        for implementation in &mut ast.inherent_implementations {
            // A nominal receiver is named; others (`[]T`, `rc T`, `gc T`) use their
            // spelling, so an impl block is addressed by what it extends.
            let owner = match &implementation.receiver {
                Type::Named(owner) | Type::Applied(owner, _) => owner.to_string(),
                other => other.to_string(),
            };
            for method in &mut implementation.methods {
                let path = format!("{module}::{owner}::{}", method.function.name);
                let span = method.function.span;
                attach(self, path, &mut method.function.annotations, span)?;
            }
        }
        Ok(())
    }
    /// Every provider binding must have found its declaration.
    fn check_bindings_applied(&self) -> Result<()> {
        for (path, _) in self.bindings.iter() {
            if !self.applied_bindings.contains(path) {
                return Err(invalid_declaration(
                    DeclarationError::UnusedBinding(path.into()),
                    Span::default(),
                ));
            }
        }
        Ok(())
    }
    fn declarations(&mut self, start: usize) -> Result<()> {
        let mut canonical = HashMap::new();
        let mut next_declaration = self.next_declaration;
        for (index, node) in self.nodes.iter_mut().enumerate().skip(start) {
            // Declaration lookup below is by name, once per declared item. Index each
            // declaration list and the export list once per module, keeping the first
            // declaration for a repeated name as the former linear `find` did.
            let exports: HashSet<&str> = node.ast.exports.iter().map(|e| &**e).collect();
            let exported =
                |prefix: &str, name: &str| exports.contains(format!("{prefix}{name}").as_str());
            let mut definitions: Vec<(Box<str>, Kind, bool)> = node
                .ast
                .functions
                .iter()
                .map(|f| (f.name.clone(), Kind::Value, exports.contains(&*f.name)))
                .collect();
            definitions.extend(node.ast.type_aliases.iter().map(|alias| {
                (
                    alias.name.clone(),
                    Kind::Type,
                    exported("$type$", &alias.name),
                )
            }));
            definitions.extend(
                node.ast
                    .enums
                    .iter()
                    .map(|e| (e.name.clone(), Kind::Type, exported("$type$", &e.name))),
            );
            definitions.extend(
                node.ast
                    .records
                    .iter()
                    .map(|r| (r.name.clone(), Kind::Type, exported("$type$", &r.name))),
            );
            definitions.extend(
                node.ast
                    .worlds
                    .iter()
                    .map(|w| (w.name.clone(), Kind::World, exported("$world$", &w.name))),
            );
            definitions.extend(
                node.ast
                    .traits
                    .iter()
                    .map(|t| (t.name.clone(), Kind::Type, exported("$type$", &t.name))),
            );
            definitions.extend(node.ast.properties.iter().map(|p| {
                (
                    p.name.clone(),
                    Kind::Property,
                    exported("$property$", &p.name),
                )
            }));
            definitions.extend(node.ast.body.iter().filter_map(|s| match &s.kind {
                StmtKind::Constant(n, _) | StmtKind::Declare(n, _) => {
                    Some((n.clone(), Kind::Value, exports.contains(&**n)))
                }
                _ => None,
            }));
            let functions = first_by_name(&node.ast.functions, |f| &f.name);
            let records = first_by_name(&node.ast.records, |r| &r.name);
            let enums = first_by_name(&node.ast.enums, |e| &e.name);
            let traits = first_by_name(&node.ast.traits, |t| &t.name);
            let aliases = first_by_name(&node.ast.type_aliases, |a| &a.name);
            let opaque_records: HashSet<&str> = node
                .ast
                .records
                .iter()
                .filter(|r| r.opaque)
                .map(|r| &*r.name)
                .collect();
            for (name, kind, public) in definitions {
                let (annotations, declaration_kind, span) = match kind {
                    Kind::Value => functions
                        .get(&*name)
                        .map(|f| (&f.annotations[..], DeclarationKind::Function, f.span)),
                    Kind::Type => records
                        .get(&*name)
                        .map(|r| (&r.annotations[..], DeclarationKind::Record, r.span))
                        .or_else(|| {
                            enums
                                .get(&*name)
                                .map(|e| (&e.annotations[..], DeclarationKind::Enum, e.span))
                        })
                        .or_else(|| {
                            traits
                                .get(&*name)
                                .map(|t| (&t.annotations[..], DeclarationKind::Trait, t.span))
                        }),
                    _ => None,
                }
                .unwrap_or((&[], DeclarationKind::Function, Span::default()));
                let semantic = declarations::bind(annotations, node.trust, declaration_kind)?;
                // An engine-known canonical declaration exists once. Opaque host types
                // have no engine identity: many declare the same traits.
                if let Some(
                    semantic @ (InternalSemantic::Type(_)
                    | InternalSemantic::TypeFamily(_)
                    | InternalSemantic::Trait(_)
                    | InternalSemantic::AllocatorTrait),
                ) = semantic
                    && !matches!(semantic, InternalSemantic::Type(NativeType::Opaque(_)))
                    && let Some(previous) = canonical.insert(semantic, span)
                {
                    return Err(
                        invalid_declaration(DeclarationError::DuplicateCanonical, span)
                            .with_related(previous, "first declaration"),
                    );
                }
                if let Some(record) = records.get(&*name) {
                    let expected = match semantic {
                        Some(InternalSemantic::Type(NativeType::Opaque(_))) => {
                            Some(record.type_params.len())
                        }
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
                if let Some(function) = functions.get(&*name) {
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
                        (None, FunctionBodyKind::Declaration) if !node.interface => {
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
                if kind == Kind::Type && opaque_records.contains(&*name) {
                    self.opaque.insert(id);
                }
                if let Some(InternalSemantic::Type(NativeType::Opaque(_))) = semantic
                    && let Some(record) = records.get(&*name)
                {
                    self.opaque_arity.insert(id, record.type_params.len());
                }
                let entries = node.names.entry(name.to_string()).or_default();
                if entries.iter().any(|s| s.kind == kind) {
                    return Err(error(
                        format!("duplicate declaration `{}`", cyallo_ast::source_name(&name)),
                        Span::default(),
                    ));
                }
                let target: Box<str> = format!("$module{index}${kind:?}${name}").into();
                self.source_names.insert(target.clone(), name.clone());
                if let Some(alias) = aliases.get(&*name) {
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
        self.enum_declarations(start)?;
        self.associated_declarations(start)?;
        self.interface_implementations(start)?;
        self.world_declarations(start)?;
        Ok(())
    }
    /// Bind each interface's opaque records to the implementing module's types.
    /// An opaque record is bound exactly once; inside the linked program the
    /// binding is the type, so it reuses the type-alias path.
    fn interface_implementations(&mut self, start: usize) -> Result<()> {
        for module in start..self.nodes.len() {
            let implementations =
                std::mem::take(&mut self.nodes[module].ast.interface_implementations);
            for implementation in implementations {
                let diagnostic = |error| invalid_declaration(error, implementation.span);
                let interface = self
                    .path(module, &implementation.interface, Some(Kind::Module))
                    .and_then(|symbol| {
                        self.nodes
                            .iter()
                            .position(|n| n.path == symbol.target.as_ref())
                    })
                    .ok_or_else(|| {
                        diagnostic(DeclarationError::UnresolvedInterface(
                            implementation.interface.clone(),
                        ))
                    })?;
                if !self.nodes[interface].interface {
                    return Err(diagnostic(DeclarationError::NotInterfaceModule(
                        implementation.interface.clone(),
                    )));
                }
                let _implementer = self
                    .path(module, &implementation.module, Some(Kind::Module))
                    .and_then(|symbol| {
                        self.nodes
                            .iter()
                            .position(|n| n.path == symbol.target.as_ref())
                    })
                    .ok_or_else(|| {
                        diagnostic(DeclarationError::UnresolvedImplementer(
                            implementation.module.clone(),
                        ))
                    })?;
                if !self.implemented_interfaces.contains(&interface) {
                    self.implemented_interfaces.push(interface);
                }
                for (name, ty) in implementation.bindings {
                    let not_opaque = || {
                        diagnostic(DeclarationError::NotOpaqueType {
                            interface: implementation.interface.clone(),
                            name: name.clone(),
                        })
                    };
                    let record = self.nodes[interface]
                        .ast
                        .records
                        .iter()
                        .find(|r| r.name == name && r.opaque)
                        .ok_or_else(not_opaque)?;
                    if !record.type_params.is_empty() {
                        return Err(diagnostic(DeclarationError::GenericOpaqueBinding(name)));
                    }
                    let symbol = self.nodes[interface]
                        .names
                        .get(name.as_ref())
                        .and_then(|symbols| symbols.iter().find(|s| s.kind == Kind::Type))
                        .cloned()
                        .ok_or_else(not_opaque)?;
                    if let Some(previous) = self.bound_opaque.insert(symbol.id, implementation.span)
                    {
                        return Err(diagnostic(DeclarationError::DuplicateOpaqueBinding(name))
                            .with_related(previous, "first binding"));
                    }
                    self.aliases.insert(symbol.target, (module, ty));
                }
            }
        }
        // Every opaque type of an implemented interface needs a binding, and every
        // binding must name a resolvable, acyclic type in its implementer.
        for &interface in &self.implemented_interfaces {
            for record in self.nodes[interface]
                .ast
                .records
                .iter()
                .filter(|r| r.opaque)
            {
                let symbol = self.nodes[interface].names[record.name.as_ref()]
                    .iter()
                    .find(|s| s.kind == Kind::Type)
                    .expect("declared opaque record");
                if !self.bound_opaque.contains_key(&symbol.id) {
                    return Err(invalid_declaration(
                        DeclarationError::MissingOpaqueBinding {
                            interface: self.nodes[interface].path.clone().into(),
                            name: record.name.clone(),
                        },
                        record.span,
                    ));
                }
                let mut resolved = Type::Named(symbol.target.clone());
                self.ty_resolved_target(&mut resolved, record.span)?;
            }
        }
        Ok(())
    }
    /// A module path relative to a package root, or the full path outside it.
    fn relative_path(&self, path: &str, package: usize) -> String {
        let root = self.nodes[package].path.as_str();
        match path.strip_prefix(root) {
            Some("") => String::new(),
            Some(rest) if rest.starts_with("::") => rest[2..].to_owned(),
            _ => path.to_owned(),
        }
    }
    /// Stable spelling of a resolved target: declaring module path plus source name,
    /// relative to `package` when the module belongs to it. Independent of
    /// declaration numbering and of how the declaration was reached.
    fn canonical_target(&self, target: &str, package: usize) -> String {
        let Some(rest) = target.strip_prefix("$module") else {
            return target.to_owned();
        };
        let mut parts = rest.split('$');
        let (Some(index), Some(_kind), Some(name)) = (
            parts.next().and_then(|n| n.parse::<usize>().ok()),
            parts.next(),
            parts.next(),
        ) else {
            return target.to_owned();
        };
        let Some(node) = self.nodes.get(index) else {
            return target.to_owned();
        };
        let module = if node.package == package {
            self.relative_path(&node.path, package)
        } else {
            node.path.clone()
        };
        if module.is_empty() {
            name.to_owned()
        } else {
            format!("{module}::{name}")
        }
    }
    fn canonical_type(&self, ty: &Type, package: usize) -> String {
        let mut ty = ty.clone();
        cyallo_ast::visit::visit_type(&mut ty, &mut |ty| match ty {
            Type::Named(name)
            | Type::TraitObject(name)
            | Type::RecordRef(name)
            | Type::Record(name, _)
            | Type::Applied(name, _) => *name = self.canonical_target(name, package).into(),
            _ => {}
        });
        ty.to_string()
    }
    /// Signature text without parameter names, bodies, annotations or spans.
    fn canonical_signature(&self, function: &Function, package: usize) -> String {
        let generics = function.type_params.join(",");
        let constraints: Vec<_> = function
            .constraints
            .iter()
            .map(|constraint| {
                let bounds: Vec<_> = constraint
                    .bounds
                    .iter()
                    .map(|bound| self.canonical_type(bound, package))
                    .collect();
                format!("{}:{}", constraint.parameter, bounds.join("+"))
            })
            .collect();
        let parameters: Vec<_> = function
            .params
            .iter()
            .map(|p| {
                format!(
                    "{:?}/{:?}{} {}",
                    p.passing,
                    p.arity,
                    if p.receiver { "/self" } else { "" },
                    self.canonical_type(&p.ty, package)
                )
            })
            .collect();
        format!(
            "{:?}<{generics}>[{}]({}) -> {}",
            function.kind,
            constraints.join(";"),
            parameters.join(", "),
            self.canonical_type(&function.result, package)
        )
    }
    /// Public interface metadata and the resolved identities for its bindable items.
    fn interface_items(
        &self,
        index: usize,
        ast: &Ast,
        declarations: &mut Vec<(Box<str>, DeclarationId)>,
    ) -> Result<Vec<crate::sdk::InterfaceItem>> {
        use crate::sdk::{InterfaceItem, ItemKind, SdkError, sdk_diagnostic, since};
        let node = &self.nodes[index];
        let package = node.package;
        let module = self.relative_path(&node.path, package);
        let path = |name: &str| {
            if module.is_empty() {
                name.to_owned()
            } else {
                format!("{module}::{name}")
            }
        };
        let source = |target: &str| -> String {
            self.source_names
                .get(target)
                .map(|name| name.to_string())
                .unwrap_or_else(|| target.to_owned())
        };
        let exported = |name: &str, namespace: &str| {
            ast.exports
                .iter()
                .any(|e| e.as_ref() == format!("{namespace}{name}"))
        };
        let version = |annotations: &[DeclarationAnnotation], span: Span| {
            since(annotations).map_err(|e: SdkError| sdk_diagnostic(e, span))
        };
        let declaration_ids: HashMap<_, _> = node
            .names
            .values()
            .flatten()
            .filter(|symbol| symbol.defining_module == index)
            .map(|symbol| (symbol.target.as_ref(), symbol.id))
            .collect();
        let declaration_id = |target: &str, span: Span| {
            use crate::module_graph::{ModuleError, module_diagnostic};
            declaration_ids.get(target).copied().ok_or_else(|| {
                module_diagnostic(ModuleError::MissingSymbol {
                    symbol: target.into(),
                    span,
                })
            })
        };
        let mut items = vec![];
        for function in &ast.functions {
            let name = source(&function.name);
            if !exported(&name, "") {
                continue;
            }
            let path = path(&name);
            let bodyless = function.body_kind == FunctionBodyKind::Declaration;
            if bodyless {
                declarations.push((
                    path.as_str().into(),
                    declaration_id(&function.name, function.span)?,
                ));
            }
            items.push(
                InterfaceItem::new(
                    &path,
                    ItemKind::Function,
                    version(&function.annotations, function.span)?,
                    self.canonical_signature(function, package),
                )
                .bindable(bodyless, Some(function.params.len())),
            );
        }
        for record in &ast.records {
            let name = source(&record.name);
            if !exported(&name, "$type$") {
                continue;
            }
            let fields: Vec<_> = record
                .fields
                .iter()
                .map(|(field, ty)| format!("{field}: {}", self.canonical_type(ty, package)))
                .collect();
            let (kind, text) = if record.opaque {
                (
                    ItemKind::OpaqueRecord,
                    format!("opaque<{}>", record.type_params.len()),
                )
            } else {
                (
                    ItemKind::Record,
                    format!(
                        "<{}>{{{}}}",
                        record.type_params.join(","),
                        fields.join(", ")
                    ),
                )
            };
            let path = path(&name);
            if record.opaque {
                declarations.push((
                    path.as_str().into(),
                    declaration_id(&record.name, record.span)?,
                ));
            }
            items.push(
                InterfaceItem::new(
                    &path,
                    kind,
                    version(&record.annotations, record.span)?,
                    text,
                )
                .bindable(record.opaque, None),
            );
        }
        for declaration in &ast.enums {
            let name = source(&declaration.name);
            if !exported(&name, "$type$") {
                continue;
            }
            let mut variants = vec![];
            for variant in &declaration.variants {
                let payload = variant
                    .payload
                    .as_ref()
                    .map_or(String::new(), |ty| self.canonical_type(ty, package));
                let introduced = version(&variant.annotations, variant.span)?
                    .map_or(String::new(), |v| format!("@{v}"));
                variants.push(format!("{}{introduced}: {payload}", variant.name));
            }
            items.push(InterfaceItem::new(
                &path(&name),
                ItemKind::Enum,
                version(&declaration.annotations, declaration.span)?,
                format!(
                    "<{}>{}{{{}}}",
                    declaration.type_params.join(","),
                    if declaration.open { " open" } else { "" },
                    variants.join(", ")
                ),
            ));
        }
        for trait_ in &ast.traits {
            let name = source(&trait_.name);
            if !exported(&name, "$type$") {
                continue;
            }
            let methods: Vec<_> = trait_
                .methods
                .iter()
                .map(|m| {
                    format!(
                        "{}: {}",
                        source(&m.name),
                        self.canonical_signature(m, package)
                    )
                })
                .collect();
            items.push(InterfaceItem::new(
                &path(&name),
                ItemKind::Trait,
                version(&trait_.annotations, trait_.span)?,
                format!(
                    "<{}>{{{}}}",
                    trait_.type_params.join(","),
                    methods.join("; ")
                ),
            ));
        }
        for alias in &ast.type_aliases {
            if !exported(&alias.name, "$type$") {
                continue;
            }
            items.push(InterfaceItem::new(
                &path(&alias.name),
                ItemKind::Alias,
                None,
                self.canonical_type(&alias.ty, package),
            ));
        }
        for method in self
            .interface_methods
            .iter()
            .filter(|m| m.module == index && m.public)
        {
            let name = source(&method.function.name);
            let path = format!("{}::{name}", method.owner);
            let bodyless = method.function.body_kind == FunctionBodyKind::Declaration;
            if bodyless {
                declarations.push((path.as_str().into(), method.id));
            }
            items.push(
                InterfaceItem::new(
                    &path,
                    ItemKind::Method,
                    version(&method.function.annotations, method.function.span)?,
                    self.canonical_signature(&method.function, package),
                )
                .bindable(bodyless, Some(method.function.params.len())),
            );
        }
        Ok(items)
    }
    /// Resolve worlds: expand `include`, find imported modules, check member lists
    /// against the real signatures, and resolve exported signatures.
    fn world_declarations(&mut self, start: usize) -> Result<()> {
        use crate::module_graph::{ResolvedWorld, ResolvedWorldExport, ResolvedWorldImport};
        struct Pending {
            module: usize,
            world: World,
            id: DeclarationId,
        }
        let mut pending: Vec<Pending> = vec![];
        for module in start..self.nodes.len() {
            for world in std::mem::take(&mut self.nodes[module].ast.worlds) {
                let id = self.nodes[module].names[world.name.as_ref()]
                    .iter()
                    .find(|s| s.kind == Kind::World)
                    .expect("declared world")
                    .id;
                pending.push(Pending { module, world, id });
            }
        }
        let mut done: HashMap<DeclarationId, usize> = self
            .worlds
            .iter()
            .enumerate()
            .map(|(index, world)| (world.id, index))
            .collect();
        fn resolve(
            this: &mut Resolver,
            pending: &[Pending],
            index: usize,
            done: &mut HashMap<DeclarationId, usize>,
            stack: &mut Vec<DeclarationId>,
        ) -> Result<usize> {
            let Pending { module, world, id } = &pending[index];
            let (module, id) = (*module, *id);
            if let Some(&resolved) = done.get(&id) {
                return Ok(resolved);
            }
            let fail = |error: DeclarationError, span: Span| invalid_declaration(error, span);
            if stack.contains(&id) {
                return Err(fail(
                    DeclarationError::CyclicWorldInclude(world.name.clone()),
                    world.span,
                ));
            }
            stack.push(id);
            let mut resolved = ResolvedWorld {
                id,
                module: ModuleId(this.nodes[module].id),
                path: format!("{}::{}", this.nodes[module].path, world.name).into(),
                includes: vec![],
                imports: vec![],
                exports: vec![],
                digest: crate::sdk::Digest([0; 32]),
            };
            let add_import = |resolved: &mut ResolvedWorld,
                              import: ResolvedWorldImport,
                              span: Span|
             -> Result<()> {
                if let Some(existing) = resolved.imports.iter().find(|i| i.field == import.field) {
                    if existing.origin == import.origin
                        && existing.module == import.module
                        && import.origin != id
                    {
                        return Ok(());
                    }
                    return Err(fail(
                        DeclarationError::DuplicateWorldField(import.field.clone()),
                        span,
                    ));
                }
                resolved.imports.push(import);
                Ok(())
            };
            let add_export = |resolved: &mut ResolvedWorld,
                              export: ResolvedWorldExport,
                              span: Span|
             -> Result<()> {
                if let Some(existing) = resolved.exports.iter().find(|e| e.name == export.name) {
                    if existing.origin == export.origin && export.origin != id {
                        return Ok(());
                    }
                    return Err(fail(
                        DeclarationError::DuplicateWorldExport(export.name.clone()),
                        span,
                    ));
                }
                resolved.exports.push(export);
                Ok(())
            };
            for item in &world.items {
                match item {
                    WorldItem::Include { path, span } => {
                        let symbol =
                            this.path(module, path, Some(Kind::World)).ok_or_else(|| {
                                fail(DeclarationError::UnresolvedWorld(path.clone()), *span)
                            })?;
                        let included = match pending.iter().position(|p| p.id == symbol.id) {
                            Some(position) => resolve(this, pending, position, done, stack)?,
                            None => *done.get(&symbol.id).ok_or_else(|| {
                                fail(DeclarationError::UnresolvedWorld(path.clone()), *span)
                            })?,
                        };
                        let included = if included < this.worlds.len() {
                            this.worlds[included].clone()
                        } else {
                            // Resolved earlier in this batch and not yet published.
                            unreachable!("included world is published before use")
                        };
                        resolved.includes.push(included.id);
                        for import in included.imports {
                            add_import(&mut resolved, import, *span)?;
                        }
                        for export in included.exports {
                            add_export(&mut resolved, export, *span)?;
                        }
                    }
                    WorldItem::Import {
                        path,
                        alias,
                        members,
                        span,
                    } => {
                        let symbol =
                            this.path(module, path, Some(Kind::Module)).ok_or_else(|| {
                                fail(DeclarationError::UnresolvedWorldImport(path.clone()), *span)
                            })?;
                        this.check_import(module, &symbol, *span)?;
                        let target = this
                            .nodes
                            .iter()
                            .position(|n| n.path == symbol.target.as_ref())
                            .ok_or_else(|| {
                                fail(DeclarationError::UnresolvedWorldImport(path.clone()), *span)
                            })?;
                        let field = alias
                            .clone()
                            .unwrap_or_else(|| path.rsplit("::").next().unwrap_or(path).into());
                        let mut listed = None;
                        let mut signatures = vec![];
                        if let Some(members) = members {
                            let mut names = vec![];
                            for member in members {
                                signatures.push(
                                    this.check_world_member(module, target, member, path)?
                                        .into(),
                                );
                                names.push(member.name.clone());
                            }
                            listed = Some(names);
                        }
                        add_import(
                            &mut resolved,
                            ResolvedWorldImport {
                                origin: id,
                                field,
                                module: ModuleId(this.nodes[target].id),
                                path: symbol.target.clone(),
                                members: listed,
                                member_signatures: signatures,
                            },
                            *span,
                        )?;
                    }
                    WorldItem::Export { function } => {
                        let mut signature = function.clone();
                        for parameter in &mut signature.params {
                            this.ty(
                                module,
                                &mut parameter.ty,
                                &function.type_params,
                                function.span,
                            )?;
                        }
                        this.ty(
                            module,
                            &mut signature.result,
                            &function.type_params,
                            function.span,
                        )?;
                        add_export(
                            &mut resolved,
                            ResolvedWorldExport {
                                origin: id,
                                name: function.name.clone(),
                                signature,
                            },
                            function.span,
                        )?;
                    }
                }
            }
            stack.pop();
            let package = this.nodes[module].package;
            let mut lines: Vec<String> = resolved
                .imports
                .iter()
                .map(|import| {
                    let mut signatures: Vec<_> = import
                        .member_signatures
                        .iter()
                        .map(|s| s.as_ref())
                        .collect();
                    signatures.sort_unstable();
                    format!(
                        "import {} {} {}",
                        import.field,
                        this.relative_path(&import.path, package),
                        if import.members.is_some() {
                            signatures.join(" | ")
                        } else {
                            "*".into()
                        }
                    )
                })
                .chain(resolved.exports.iter().map(|export| {
                    format!(
                        "export {} {}",
                        export.name,
                        this.canonical_signature(&export.signature, package)
                    )
                }))
                .collect();
            lines.sort();
            resolved.digest = crate::sdk::Digest::of(&lines.join("\n"));
            this.worlds.push(resolved);
            let position = this.worlds.len() - 1;
            done.insert(id, position);
            Ok(position)
        }
        for index in 0..pending.len() {
            resolve(self, &pending, index, &mut done, &mut vec![])?;
        }
        Ok(())
    }
    /// A world import's member must be a public routine of the module with exactly
    /// the declared signature: a world may narrow a module, never change it.
    fn check_world_member(
        &self,
        from: usize,
        target: usize,
        member: &Function,
        path: &str,
    ) -> Result<String> {
        let mismatch = || {
            invalid_declaration(
                DeclarationError::WorldMemberMismatch {
                    module: path.into(),
                    name: member.name.clone(),
                },
                member.span,
            )
        };
        let missing = || {
            invalid_declaration(
                DeclarationError::WorldMemberMissing {
                    module: path.into(),
                    name: member.name.clone(),
                },
                member.span,
            )
        };
        self.nodes[target]
            .names
            .get(member.name.as_ref())
            .and_then(|symbols| symbols.iter().find(|s| s.kind == Kind::Value))
            .filter(|symbol| self.accessible(from, symbol))
            .ok_or_else(missing)?;
        let actual = self.nodes[target]
            .ast
            .functions
            .iter()
            .find(|f| f.name == member.name)
            .ok_or_else(missing)?;
        if actual.kind != member.kind
            || actual.params.len() != member.params.len()
            || actual.type_params.len() != member.type_params.len()
        {
            return Err(mismatch());
        }
        let resolved = |module: usize, function: &Function| -> Result<Function> {
            let mut function = function.clone();
            for parameter in &mut function.params {
                self.ty(
                    module,
                    &mut parameter.ty,
                    &function.type_params,
                    function.span,
                )?;
            }
            self.ty(
                module,
                &mut function.result,
                &function.type_params,
                function.span,
            )?;
            Ok(function)
        };
        // Generic parameters keep positional meaning: rename to the target's names
        // so `fn f<T>(x: T)` and `fn f<U>(x: U)` agree.
        let mut renamed = member.clone();
        for (from_name, to_name) in member.type_params.iter().zip(&actual.type_params) {
            let (from_name, to_name) = (from_name.clone(), to_name.clone());
            let rename = |ty: &mut Type| {
                cyallo_ast::visit::visit_type(ty, &mut |ty| {
                    if matches!(ty, Type::Named(n) if *n == from_name) {
                        *ty = Type::Named(to_name.clone());
                    }
                })
            };
            for parameter in &mut renamed.params {
                rename(&mut parameter.ty);
            }
            rename(&mut renamed.result);
        }
        renamed.type_params = actual.type_params.clone();
        let package = self.nodes[target].package;
        let wanted = self.canonical_signature(&resolved(from, &renamed)?, package);
        let found = self.canonical_signature(&resolved(target, actual)?, package);
        if wanted != found {
            return Err(mismatch());
        }
        Ok(found)
    }
    /// Resolve a type already named by a resolved target, to validate alias chains.
    fn ty_resolved_target(&self, ty: &mut Type, span: Span) -> Result<()> {
        let Type::Named(target) = ty else {
            return Ok(());
        };
        let mut seen = vec![];
        let mut target = target.clone();
        while let Some((owner, alias)) = self.aliases.get(&target) {
            if seen.contains(&target) {
                return Err(cyallo_ast::diagnostic::cyclic_type_alias(span));
            }
            seen.push(target.clone());
            let mut alias = alias.clone();
            self.ty(*owner, &mut alias, &[], span)?;
            match alias {
                Type::Named(next) => target = next,
                _ => break,
            }
        }
        Ok(())
    }
    fn enum_declarations(&mut self, start: usize) -> Result<()> {
        for module in start..self.nodes.len() {
            let mut declarations = std::mem::take(&mut self.nodes[module].ast.enums);
            for declaration in &mut declarations {
                let owner = self
                    .path(module, &declaration.name, Some(Kind::Type))
                    .expect("declared enum");
                declaration.resolved = Some(ResolvedEnum {
                    declaration: EnumId(owner.id),
                    package: self.nodes[self.nodes[module].package].id,
                    arguments: vec![],
                });
                let mut seen = std::collections::HashSet::new();
                for variant in &mut declaration.variants {
                    if !seen.insert(variant.name.clone()) {
                        return Err(crate::enums::invalid_enum(
                            crate::enums::EnumError::DuplicateVariant {
                                name: variant.name.clone(),
                            },
                            variant.span,
                        ));
                    }
                    let role = declarations::bind(
                        &variant.annotations,
                        self.nodes[module].trust,
                        DeclarationKind::Variant,
                    )?;
                    // Variants of the `core` enums have reserved ids, so values built
                    // by any program or runtime agree on them.
                    let id = match role {
                        Some(InternalSemantic::Variant(encoding)) => encoding.reserved(),
                        _ => None,
                    }
                    .map_or_else(|| self.declaration_id(), |variant| variant.0);
                    let target: Box<str> =
                        format!("$variant{}${}", owner.id.0, variant.name).into();
                    self.source_names
                        .insert(target.clone(), variant.name.clone());
                    self.variant_ids.insert(id);
                    self.variants
                        .insert(id, (owner.target.clone(), variant.payload.is_some()));
                    if variant.payload.is_none() {
                        self.unit_variants.insert(id);
                    }
                    self.associated
                        .entry(owner.id)
                        .or_default()
                        .entry(variant.name.to_string())
                        .or_default()
                        .push(Symbol {
                            id,
                            native: None,
                            origin: owner.origin,
                            target: target.clone(),
                            kind: Kind::Value,
                            owner: module,
                            defining_module: module,
                            public: owner.public,
                            intrinsic: None,
                        });
                    let encoding = match declarations::bind(
                        &variant.annotations,
                        self.nodes[module].trust,
                        DeclarationKind::Variant,
                    )? {
                        Some(InternalSemantic::Variant(encoding)) => encoding,
                        _ => EnumEncoding::General,
                    };
                    if encoding != EnumEncoding::General
                        && !matches!(
                            owner.native,
                            Some(InternalSemantic::Type(NativeType::Storage(
                                StorageKind::Option
                                    | StorageKind::Result
                                    | StorageKind::GeneratorStep
                                    | StorageKind::SendError
                            )))
                        )
                        && !matches!(
                            encoding,
                            EnumEncoding::ReceiveClosed | EnumEncoding::ReceiveEmpty
                        )
                    {
                        return Err(crate::enums::invalid_enum(
                            crate::enums::EnumError::NativeContract {
                                name: declaration.name.clone(),
                            },
                            variant.span,
                        ));
                    }
                    variant.resolved = Some(ResolvedVariant {
                        encoding,
                        declaration: VariantId(id),
                        symbol: target,
                    });
                }
            }
            self.nodes[module].ast.enums = declarations;
        }
        Ok(())
    }
    fn associated_declarations(&mut self, start: usize) -> Result<()> {
        for module in start..self.nodes.len() {
            let implementations = self.nodes[module].ast.inherent_implementations.clone();
            for (implementation_index, implementation) in implementations.into_iter().enumerate() {
                let owner_name = match &implementation.receiver {
                    Type::Named(n) | Type::Applied(n, _) => Some(n.as_ref()),
                    _ => None,
                };
                let owner = owner_name.and_then(|name| self.path(module, name, Some(Kind::Type)));
                for (method_index, method) in implementation.methods.into_iter().enumerate() {
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
                    // A method belongs to its module and declaration position,
                    // independently of archive offsets and appended packages.
                    let target: Box<str> = format!(
                        "$associated{}${implementation_index}${method_index}${name}",
                        self.nodes[module].path
                    )
                    .into();
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
                    self.associated_owners.insert(id, owner.target.clone());
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
                let mut seen = std::collections::HashSet::new();
                while let Some((owner, ty)) = self.aliases.get(&symbol.target) {
                    if !seen.insert(symbol.id) {
                        return None;
                    }
                    let (Type::Named(name) | Type::Applied(name, _)) = ty else {
                        return None;
                    };
                    symbol = self.path(*owner, name, Some(Kind::Type))?;
                }
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
                let entries = if import.glob && target.kind == Kind::Type {
                    self.associated
                        .get(&target.id)
                        .into_iter()
                        .flat_map(|entries| entries.iter())
                        .flat_map(|(name, symbols)| {
                            symbols
                                .iter()
                                .filter(|s| self.variant_ids.contains(&s.id))
                                .map(move |s| (name.clone(), s.clone()))
                        })
                        .collect()
                } else if import.glob {
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
                    // A module may publish its own child module: the file gate opens
                    // with `pub use b`, as `pub mod b` would in Rust.
                    let own_child = symbol.kind == Kind::Module && symbol.owner == *module;
                    if import.public && !symbol.public && !own_child {
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
                        if own_child && existing.target == symbol.target {
                            if import.public && !existing.public {
                                existing.public = true;
                                changed = true;
                            }
                            previous.push(ordinal);
                            continue;
                        }
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
                if let Some((declaration, InternalSemantic::Type(NativeType::Opaque(traits)))) =
                    self.native_target(&target)
                {
                    let expected = self.opaque_arity.get(&declaration).copied().unwrap_or(0);
                    if expected != 0 {
                        return Err(invalid_declaration(
                            DeclarationError::GenericArity {
                                expected,
                                actual: 0,
                            },
                            span,
                        ));
                    }
                    *ty = Type::Opaque(target, vec![], traits);
                } else if let Some((_, InternalSemantic::Type(id))) = self.native_target(&target) {
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
                            | StorageKind::AllocError),
                        ) => Type::Storage(kind, Box::new(Type::Void)),
                        _ => return Err(error("generic type requires element arguments", span)),
                    };
                } else {
                    *n = target;
                }
            }
            Type::Applied(n, arguments) => {
                // A weak reference may start with an allocator lifetime, which is not
                // a type: take it out before the arguments are resolved.
                let weak_lifetime = {
                    let target = self.symbol(module, n, Kind::Type, span)?;
                    match self.native_target(&target) {
                        Some((_, InternalSemantic::Type(NativeType::Opaque(traits))))
                            if traits.weak()
                                && matches!(arguments.first(), Some(Type::Lifetime(_))) =>
                        {
                            let Type::Lifetime(lifetime) = arguments.remove(0) else {
                                unreachable!()
                            };
                            Some(lifetime)
                        }
                        _ => None,
                    }
                };
                for t in arguments.iter_mut() {
                    self.ty_alias(module, t, params, span, seen)?;
                }
                let target = self.symbol(module, n, Kind::Type, span)?;
                if let Some((declaration, InternalSemantic::TypeFamily(_))) =
                    self.native_target(&target)
                {
                    *ty = Type::Dependent(declaration, std::mem::take(arguments));
                } else if let Some((
                    declaration,
                    InternalSemantic::Type(NativeType::Opaque(traits)),
                )) = self.native_target(&target)
                {
                    let expected = self.opaque_arity.get(&declaration).copied().unwrap_or(0);
                    if traits.weak() {
                        // `Weak<'a, T>`: the owner is the `rc` of the pointee in the optional
                        // allocator region. The language builds it; the type is the provider's.
                        let lifetime = weak_lifetime;
                        if arguments.len() != 1 {
                            return Err(error(
                                "a weak reference requires a pointee type and an optional allocator lifetime",
                                span,
                            ));
                        }
                        let pointee = arguments.remove(0);
                        *ty = Type::Opaque(
                            target,
                            vec![Type::Managed {
                                kind: MemoryKind::Rc,
                                locality: Locality::Local,
                                lifetime,
                                pointee: Box::new(pointee),
                            }],
                            traits,
                        );
                        return Ok(());
                    }
                    if arguments.len() != expected {
                        return Err(invalid_declaration(
                            DeclarationError::GenericArity {
                                expected,
                                actual: arguments.len(),
                            },
                            span,
                        ));
                    }
                    *ty = Type::Opaque(target, std::mem::take(arguments), traits);
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
            Type::Opaque(_, arguments, _) => {
                for argument in arguments {
                    self.ty_alias(module, argument, params, span, seen)?;
                }
            }
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
            Type::Record(_, fs) | Type::AnonymousRecord(fs) | Type::Object(fs) => {
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
            // A collection type written as a literal head (`Vec[..]`, `Vec<T>[..]`) is
            // known by its provider-declared `collection` trait; the literal carries the
            // declared type, applied to its element type by the operation.
            let constructor = constructor.and_then(|symbol| match symbol.native {
                Some(InternalSemantic::Type(NativeType::Opaque(traits))) if traits.collection() => {
                    Some((
                        if traits.double_ended() {
                            Intrinsic::DequeLiteral
                        } else {
                            Intrinsic::VecLiteral
                        },
                        Type::Opaque(symbol.target.clone(), vec![], traits),
                    ))
                }
                _ => None,
            });
            if let Some((id, head)) = constructor {
                match &mut e.kind {
                    ExprKind::Index(_, args) => {
                        e.kind = ExprKind::IntrinsicCall(id.id(), vec![head], std::mem::take(args))
                    }
                    ExprKind::TypedCollection(_, types, args) => {
                        let mut all = vec![head];
                        all.append(types);
                        e.kind = ExprKind::IntrinsicCall(id.id(), all, std::mem::take(args))
                    }
                    _ => {}
                }
            }
            let brace = match &mut e.kind {
                ExprKind::Record(name, fields) => self
                    .path(module, name, Some(Kind::Value))
                    .filter(|s| self.variant_ids.contains(&s.id))
                    .map(|s| (s, vec![], std::mem::take(fields))),
                ExprKind::GenericRecord(name, types, fields) => self
                    .path(module, name, Some(Kind::Value))
                    .filter(|s| self.variant_ids.contains(&s.id))
                    .map(|s| (s, std::mem::take(types), std::mem::take(fields))),
                _ => None,
            };
            if let Some((symbol, mut types, mut fields)) = brace {
                self.check_import(module, &symbol, e.span)?;
                for ty in &mut types {
                    self.ty(module, ty, params, e.span)?;
                }
                for (_, value) in &mut fields {
                    self.expr(module, value, scopes, params)?;
                }
                let payload = Expr {
                    span: e.span,
                    kind: ExprKind::VariantPayload(fields),
                };
                e.kind = if types.is_empty() {
                    ExprKind::Call(symbol.target, vec![payload])
                } else {
                    ExprKind::GenericCall(symbol.target, types, vec![payload])
                };
                return Ok(());
            }
            match &mut e.kind {
                ExprKind::AnonymousRecord(fields) | ExprKind::VariantPayload(fields) => {
                    for (_, value) in fields {
                        self.expr(module, value, scopes, params)?;
                    }
                }
                ExprKind::Match(value, arms) => {
                    self.expr(module, value, scopes, params)?;
                    for arm in arms {
                        arm.package = Some(self.nodes[self.nodes[module].package].id);
                        let mut bindings = HashMap::new();
                        self.pattern(module, &mut arm.pattern, &mut bindings)?;
                        scopes.push(bindings);
                        if let Some(guard) = &mut arm.guard {
                            self.expr(module, guard, scopes, params)?;
                        }
                        match &mut arm.body {
                            MatchBody::Expression(value) => {
                                self.expr(module, value, scopes, params)?
                            }
                            MatchBody::Block(body, tail) => {
                                // Keep block locals visible to its tail expression.
                                if let Some(value) = tail.take() {
                                    body.push(Stmt {
                                        span: value.span,
                                        kind: StmtKind::Evaluate(*value),
                                    });
                                    self.block(module, body, scopes, params, false)?;
                                    let StmtKind::Evaluate(value) = body.pop().unwrap().kind else {
                                        unreachable!()
                                    };
                                    *tail = Some(Box::new(value));
                                } else {
                                    self.block(module, body, scopes, params, false)?;
                                }
                            }
                        }
                        scopes.pop();
                    }
                }
                ExprKind::EnumConstruct(_, ty, payload) => {
                    self.ty(module, ty, params, e.span)?;
                    if let Some(payload) = payload {
                        self.expr(module, payload, scopes, params)?;
                    }
                }
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
                        if self.unit_variants.contains(&symbol.id) {
                            e.kind = ExprKind::Call(symbol.target, vec![]);
                            return Ok(());
                        }
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
                        if self.unit_variants.contains(&symbol.id) {
                            return Err(crate::enums::invalid_enum(
                                crate::enums::EnumError::UnitVariantCalled { name: n.clone() },
                                e.span,
                            ));
                        }
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
                        if self.opaque.contains(&symbol.id) {
                            return Err(invalid_declaration(
                                DeclarationError::OpaqueRecordConstruction,
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
                if let StmtKind::IfLet(pattern, value, yes, no) = &mut s.kind {
                    let value = std::mem::replace(
                        value,
                        Expr {
                            kind: ExprKind::Tuple(vec![]),
                            span: s.span,
                        },
                    );
                    s.kind = StmtKind::Evaluate(Expr {
                        span: s.span,
                        kind: ExprKind::Match(
                            Box::new(value),
                            vec![
                                MatchArm {
                                    package: None,
                                    pattern: pattern.clone(),
                                    guard: None,
                                    body: MatchBody::Block(std::mem::take(yes), None),
                                    span: s.span,
                                },
                                MatchArm {
                                    package: None,
                                    pattern: Pattern {
                                        kind: PatternKind::Wildcard,
                                        span: s.span,
                                    },
                                    guard: None,
                                    body: MatchBody::Block(std::mem::take(no), None),
                                    span: s.span,
                                },
                            ],
                        ),
                    });
                }
                match &mut s.kind {
                    StmtKind::IfLet(..) => {
                        return Err(crate::patterns::invalid_pattern(
                            crate::patterns::PatternError::UnresolvedPattern,
                            s.span,
                        ));
                    }
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
    fn finish(mut self, entry: usize) -> Result<ResolvedProgram> {
        self.declarations(entry)?;
        self.imports()?;
        let entry_package = self.nodes[entry].package;
        let mut linked = Ast::default();
        let mut associated: Vec<Vec<Function>> = (0..self.nodes.len()).map(|_| vec![]).collect();
        let mut modules = vec![];
        let mut digest_items: Vec<crate::sdk::InterfaceItem> = vec![];
        let mut interface_declarations = vec![];
        for (module, mut declaration) in std::mem::take(&mut self.associated_functions) {
            self.function(module, &mut declaration.function, &[], &[])?;
            if self.nodes[module].interface {
                let pkg = self.nodes[module].package;
                let owner = self
                    .associated_owners
                    .get(&declaration.id)
                    .map(|target| self.canonical_target(target, pkg))
                    .unwrap_or_default();
                self.interface_methods.push(InterfaceMethod {
                    module,
                    id: declaration.id,
                    owner,
                    public: true,
                    function: declaration.function,
                });
                continue;
            }
            if declaration.function.body_kind == FunctionBodyKind::Source
                && declarations::internal_annotation(&declaration.function.annotations).is_none()
            {
                associated[module].push(declaration.function);
            } else {
                linked.native.functions.push(declaration);
            }
        }
        self.records = self.record_table();
        self.enums = self.enum_table();
        self.generic_enums = self.generic_enum_table();
        self.bound_traits = self.bound_trait_table();
        self.user_traits = self.user_trait_table();
        self.plain = self.plain_functions();
        for index in 0..self.nodes.len() {
            self.next_binding = 0;
            let mut ast = std::mem::take(&mut self.nodes[index].ast);
            for declaration in &mut ast.enums {
                declaration.name =
                    self.symbol(index, &declaration.name, Kind::Type, declaration.span)?;
                for variant in &mut declaration.variants {
                    if let Some(ty) = &mut variant.payload {
                        self.ty(index, ty, &declaration.type_params, variant.span)?;
                    }
                }
            }
            for record in &mut ast.records {
                let symbol = self.path(index, &record.name, Some(Kind::Type)).unwrap();
                record.name = symbol.target;
                for (_, t) in &mut record.fields {
                    self.ty(index, t, &record.type_params, record.span)?;
                }
            }
            for f in &mut ast.functions {
                let mut generic_bounds = None;
                f.name = self.symbol(index, &f.name, Kind::Value, f.span)?;
                if f.origin == RoutineOrigin::Source
                    && let Some(body) = self.indexed.remove(&f.span)
                {
                    let bounds = self.declared_bounds(index, f);
                    match self.bind_indexed(index, f, body)? {
                        Ok(bound) => {
                            generic_bounds = bounds;
                            // A generic routine keeps an owned body too: instances the
                            // pipeline does not take are cloned from it by the specializer.
                            if !f.type_params.is_empty() {
                                f.body = bound.materialize_owned()?;
                            }
                            self.bound.insert(f.name.clone(), bound);
                        }
                        // The indexed pipeline cannot take this routine: link its
                        // owned body so the existing resolver reports or resolves it.
                        Err(body) => {
                            if !f.constraints.is_empty() {
                                return Err(declarations::unsupported_source(
                                    declarations::SourceFeature::GenericConstraints,
                                    f.span,
                                ));
                            }
                            f.body = body.materialize_owned()?
                        }
                    }
                }
                self.function(index, f, &[], &[])?;
                // The template is checked once with its type parameters opaque, against the
                // signature as linked.
                if !f.type_params.is_empty()
                    && let Some(bound) = self.bound.get_mut(&f.name)
                {
                    let (bounds, user_traits) = generic_bounds.take().unwrap_or_default();
                    bound.set_generic_header(&f.params, &f.result, bounds, user_traits);
                }
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
                    if self.nodes[index].interface {
                        let mut function = method.function.clone();
                        for param in &mut function.params {
                            cyallo_ast::visit::visit_type(&mut param.ty, &mut |ty| {
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
                        self.function(index, &mut function, &[], &implementation.type_params)?;
                        let pkg = self.nodes[index].package;
                        let owner = self.canonical_type(&receiver, pkg);
                        let id = self.declaration_id();
                        self.interface_methods.push(InterfaceMethod {
                            module: index,
                            id,
                            owner,
                            public: method.public,
                            function,
                        });
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
                                *ty = implementation.receiver.clone();
                            }
                        });
                    }
                    cyallo_ast::visit::visit_type(&mut function.result, &mut |ty| {
                        if *ty == Type::SelfType {
                            *ty = implementation.receiver.clone();
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
                if declarations::internal_annotation(&function.annotations).is_none() {
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
                            annotation: declarations::internal_annotation(&record.annotations)
                                .expect("bound native type")
                                .clone(),
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
            let strict = self.entry_mode == crate::EntryMode::Main;
            if strict {
                // An immutable `let` at the top level is a compile-time constant.
                for statement in &mut ast.body {
                    if let StmtKind::Binding(_, _, constant @ false, false, _) = &mut statement.kind
                    {
                        *constant = true;
                    }
                }
            }
            if (index != entry || strict)
                && ast.body.iter().any(|s| {
                    !matches!(
                        s.kind,
                        StmtKind::Binding(_, _, true, _, _) | StmtKind::Constant(..)
                    )
                })
            {
                return Err(error(
                    if strict {
                        "top level allows declarations, `const` and immutable `let`; \
                         put statements in `proc main()`"
                    } else {
                        "module initialization requires constants or an explicit procedure"
                    },
                    ast.body[0].span,
                ));
            }
            if strict && index == entry {
                // The entry file's program is its `proc main()`.
                let main_span = ast
                    .functions
                    .iter()
                    .find(|f| {
                        cyallo_ast::source_name(&f.name) == "main"
                            && f.kind == RoutineKind::Procedure
                            && f.params.is_empty()
                            && f.result == Type::Void
                    })
                    .map(|f| f.span);
                match main_span {
                    Some(span) => ast.body.push(Stmt {
                        kind: StmtKind::Call("main".into(), Vec::new()),
                        span,
                    }),
                    None => {
                        return Err(error(
                            "the entry file must declare `proc main()`",
                            ast.body.first().map_or(Span::default(), |s| s.span),
                        ));
                    }
                }
            }
            self.block(index, &mut ast.body, &mut Vec::new(), &[], true)?;
            ast.functions.append(&mut associated[index]);
            for declaration in &ast.enums {
                ast.functions
                    .extend(crate::enums::constructors(declaration)?);
            }
            ast.enums.retain(|declaration| {
                if let Some((id, InternalSemantic::Type(NativeType::Storage(storage)))) =
                    self.native_target(&declaration.name)
                {
                    linked.native.enums.push(NativeEnumDeclaration {
                        storage,
                        declaration: declaration.clone(),
                    });
                    linked.native.types.push(NativeTypeDeclaration {
                        id,
                        name: declaration.name.clone(),
                        parameters: declaration.type_params.clone(),
                        annotation: declaration
                            .annotations
                            .iter()
                            .find(|a| a.name.as_ref() == "cyallo_internal")
                            .unwrap()
                            .clone(),
                    });
                    false
                } else {
                    true
                }
            });
            ast.inherent_implementations.clear();
            // A bound the routine pipeline gives meaning to (`T: Numeric`) is not an
            // unimplemented feature once the body is bound to it: binding rejects a routine
            // whose bounds it does not know, so those that remain are supported.
            let bounded: Vec<_> = ast
                .functions
                .iter_mut()
                .enumerate()
                .filter(|(_, f)| !f.constraints.is_empty() && self.bound.contains_key(&f.name))
                .map(|(position, f)| (position, std::mem::take(&mut f.constraints)))
                .collect();
            let validated = declarations::validate_source_ast(&ast);
            for (position, constraints) in bounded {
                ast.functions[position].constraints = constraints;
            }
            validated?;
            let nodes = &self.nodes;
            let node = &nodes[index];
            let mut bindings: Vec<_> = node
                .names
                .iter()
                .flat_map(|(name, symbols)| {
                    symbols.iter().map(move |symbol| ModuleBinding {
                        name: name.clone().into(),
                        declaration: symbol.id,
                        defining_module: ModuleId(nodes[symbol.defining_module].id),
                        target: symbol.target.clone(),
                        kind: match symbol.kind {
                            Kind::Value => BindingKind::Value,
                            Kind::Type => BindingKind::Type,
                            Kind::Module => BindingKind::Module,
                            Kind::Property => BindingKind::Property,
                            Kind::World => BindingKind::World,
                        },
                        public: symbol.public,
                    })
                })
                .collect();
            bindings.sort_by(|a, b| a.name.cmp(&b.name).then(a.target.cmp(&b.target)));
            if node.interface && node.package == entry_package {
                digest_items.extend(self.interface_items(
                    index,
                    &ast,
                    &mut interface_declarations,
                )?);
            }
            modules.push(ResolvedModule {
                id: ModuleId(node.id),
                path: node.path.clone().into(),
                source: ast,
                bindings,
            });
        }
        let interface = crate::sdk::InterfaceDigests {
            items: digest_items,
            worlds: self
                .worlds
                .iter()
                .filter(|w| {
                    self.nodes
                        .iter()
                        .any(|n| n.id == w.module.0 && n.package == entry_package)
                })
                .map(|w| crate::sdk::WorldDigest {
                    path: self.relative_path(&w.path, entry_package).into(),
                    digest: w.digest,
                })
                .collect(),
        }
        .sorted();
        declarations::validate_native_declarations(&linked.native)?;
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
                            .filter(|symbol| {
                                !matches!(symbol.kind, Kind::Property | Kind::World)
                                    && symbol.public
                            })
                            .map(|symbol| ResolvedExport {
                                kind: match symbol.kind {
                                    Kind::Value => ExportKind::Value,
                                    Kind::Module => ExportKind::Module,
                                    Kind::Type => ExportKind::Type,
                                    Kind::Property | Kind::World => unreachable!(),
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
        // Every indexed body was left out of the owned tree, so each must be bound
        // to a linked routine or materialized. An unbound one would lose its body.
        if !self.indexed.is_empty() {
            return Err(error(
                "internal error: an indexed routine body has no linked routine",
                Span::default(),
            ));
        }
        let graph = ResolvedProgram {
            interface,
            interface_declarations,
            worlds: std::mem::take(&mut self.worlds),
            source_files: vec![],
            modules,
            entry: ModuleId(self.nodes[entry].id),
            native: linked.native,
            source_names: linked.source_names,
            exports: linked.resolved_modules,
            indexed_bodies: std::mem::take(&mut self.bound),
        };
        graph
            .constant_order()
            .map_err(crate::module_graph::module_diagnostic)?;
        Ok(graph)
    }
}
/// First item for each name, matching a linear `find` over `items`.
fn first_by_name<'a, T>(
    items: &'a [T],
    name: impl Fn(&'a T) -> &'a str,
) -> HashMap<&'a str, &'a T> {
    let mut first = HashMap::with_capacity(items.len());
    for item in items {
        first.entry(name(item)).or_insert(item);
    }
    first
}
pub fn link(asts: Vec<Ast>, paths: &[String], entry: usize) -> Result<ResolvedProgram> {
    link_with_policy(
        asts,
        paths,
        entry,
        &crate::import_policy::ImportPolicy::default(),
    )
}
/// Link a single source set with indexed routine bodies, as the compiler does.
pub fn link_with_indexed_bodies(
    asts: Vec<Ast>,
    paths: &[String],
    entry: usize,
    indexed: Vec<crate::analysis::IndexedRoutineBody>,
) -> Result<ResolvedProgram> {
    link_program(
        asts,
        paths,
        entry,
        &crate::import_policy::ImportPolicy::default(),
        &crate::core_package::CorePackage::embedded(),
        0,
        crate::EntryMode::Script,
        indexed,
    )
}
pub fn link_with_policy(
    asts: Vec<Ast>,
    paths: &[String],
    entry: usize,
    policy: &crate::import_policy::ImportPolicy,
) -> Result<ResolvedProgram> {
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
) -> Result<ResolvedProgram> {
    link_program(
        asts,
        paths,
        entry,
        policy,
        package,
        core_offset,
        crate::EntryMode::Script,
        Vec::new(),
    )
}
/// Link with an explicit entry mode. `EntryMode::Main` is for source files.
pub fn link_program(
    asts: Vec<Ast>,
    paths: &[String],
    entry: usize,
    policy: &crate::import_policy::ImportPolicy,
    package: &crate::core_package::CorePackage<'_>,
    core_offset: u32,
    mode: crate::EntryMode,
    indexed: Vec<crate::analysis::IndexedRoutineBody>,
) -> Result<ResolvedProgram> {
    let _resolve = cyallo_startup_profile::stage(cyallo_startup_profile::Stage::Resolve);
    let mut files: Vec<_> = asts.into_iter().map(Some).collect();
    let root = files[entry]
        .take()
        .ok_or_else(|| error("missing entry module", Span::default()))?;
    let dir = Path::new(&paths[entry]).parent().unwrap_or(Path::new(""));
    let mut resolver = Resolver {
        entry_mode: mode,
        indexed: indexed
            .into_iter()
            .map(|body| (body.archived_span(), body))
            .collect(),
        bound: HashMap::new(),
        plain: HashMap::new(),
        records: HashMap::new(),
        enums: HashMap::new(),
        generic_enums: HashMap::new(),
        bound_traits: HashMap::new(),
        user_traits: Default::default(),
        variants: HashMap::new(),
        aliases: HashMap::new(),
        source_names: Default::default(),
        nodes: Vec::new(),
        variant_ids: Default::default(),
        unit_variants: Default::default(),
        next_binding: 0,
        next_declaration: cyallo_ast::RESERVED_DECLARATIONS,
        prelude: None,
        packages: HashMap::new(),
        associated: HashMap::new(),
        associated_functions: vec![],
        permissions: None,
        requesting_modules: Default::default(),
        opaque: Default::default(),
        opaque_arity: HashMap::new(),
        bound_opaque: HashMap::new(),
        implemented_interfaces: vec![],
        worlds: vec![],
        associated_owners: HashMap::new(),
        bindings: package.bindings().clone(),
        applied_bindings: Default::default(),
        interface_methods: vec![],
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
            source: ModuleSource::File,
        },
    )?;
    resolver.packages.insert(core_name, core);
    // Allocate core identities before user declarations, so appending REPL
    // definitions cannot change the identity of a saved native function item.
    resolver.declarations(0)?;
    resolver.check_bindings_applied()?;
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
            source: ModuleSource::File,
        },
    )?;
    resolver.packages.insert("crate".into(), user);
    resolver.permissions =
        Some(policy.resolve(&resolver.module_graph()).map_err(|error| {
            crate::import_policy::import_policy_diagnostic(error, Span::default())
        })?);
    resolver.finish(user)
}
pub fn single(ast: Ast) -> Result<ResolvedProgram> {
    link(vec![ast], &["main.ya".into()], 0)
}

/// Replace type parameters named in `params` by the matching `arguments`.
fn substitute_named(ty: &mut Type, params: &[Box<str>], arguments: &[Type]) {
    match ty {
        Type::Named(name) => {
            if let Some(index) = params.iter().position(|param| param == name) {
                *ty = arguments[index].clone();
            }
        }
        Type::Applied(_, inner) => {
            for argument in inner {
                substitute_named(argument, params, arguments);
            }
        }
        Type::Reference { pointee, .. } => substitute_named(pointee, params, arguments),
        _ => {}
    }
}

/// A routine's bounds by type parameter, and the program's traits they name.
type DeclaredBounds = (Vec<(Box<str>, Vec<crate::generics::Bound>)>, Vec<Box<str>>);
