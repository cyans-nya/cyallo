//! Immutable, source-only syntax tables. The legacy parser can populate these
//! by moving its syntax into a builder. The reverse adapter serves consumers
//! that still require the mutable tree.

use crate::*;
use std::{collections::HashMap, fmt, marker::PhantomData, num::NonZeroU32, sync::Arc};

/// An index local to one syntax unit and one node table.
pub struct Id<T>(NonZeroU32, PhantomData<fn() -> T>);

impl<T> Copy for Id<T> {}
impl<T> Clone for Id<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&(self.0.get() - 1), f)
    }
}
impl<T> PartialEq for Id<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl<T> Eq for Id<T> {}

/// A range in the unit's contiguous list pool for `T`.
pub struct List<T> {
    start: u32,
    end: u32,
    element: PhantomData<fn() -> T>,
}

impl<T> Copy for List<T> {}
impl<T> Clone for List<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> fmt::Debug for List<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&(self.start..self.end), f)
    }
}

pub type NameId = Id<Arc<str>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexError {
    Capacity,
    SemanticNode(SemanticSyntax),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticSyntax {
    Type,
    Expression,
    VariantPattern,
    MatchArm,
    RoutineOrigin,
    EnumDeclaration,
    EnumVariant,
    TraitOrigin,
    PackageBoundary,
    ModuleIdentity,
    NativeDeclaration,
}

impl fmt::Display for SemanticSyntax {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Type => "resolved type",
            Self::Expression => "lowered expression",
            Self::VariantPattern => "resolved variant pattern",
            Self::MatchArm => "resolved match arm",
            Self::RoutineOrigin => "generated routine origin",
            Self::EnumDeclaration => "resolved enum declaration",
            Self::EnumVariant => "resolved enum variant",
            Self::TraitOrigin => "resolved trait origin",
            Self::PackageBoundary => "module package boundary",
            Self::ModuleIdentity => "resolved module identities",
            Self::NativeDeclaration => "linked native declarations",
        })
    }
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capacity => f.write_str("syntax unit exceeds the indexed AST capacity"),
            Self::SemanticNode(kind) => write!(f, "{kind} is not source syntax"),
        }
    }
}
impl std::error::Error for IndexError {}

type IndexResult<T> = Result<T, IndexError>;

pub fn index_diagnostic(error: IndexError, span: Span) -> Diagnostic {
    Diagnostic::new("E002", error.to_string(), span)
}

trait Freeze: Sized {
    type Stored;
    fn freeze(self, b: &mut Builder) -> IndexResult<Self::Stored>;
    fn thaw(v: &Self::Stored, ast: &IndexedAst) -> Self;
}

// Both adapters follow the same field mapping, so a legacy field cannot get
// silently dropped when converting a source node.
macro_rules! struct_syntax {
    ($legacy:ident => $frozen:ident { $($field:ident : $ty:ty),* $(,)? }) => {
        #[derive(Debug)]
        pub struct $frozen {
            $(pub $field: $ty),*
        }

        impl Freeze for $legacy {
            type Stored = $frozen;
            fn freeze(self, b: &mut Builder) -> IndexResult<Self::Stored> {
                let Self { $($field),* } = self;
                Ok($frozen { $($field: $field.freeze(b)?),* })
            }
            fn thaw(v: &Self::Stored, ast: &IndexedAst) -> Self {
                Self { $($field: Freeze::thaw(&v.$field, ast)),* }
            }
        }
    };
}

macro_rules! enum_syntax {
    ($legacy:ident => $frozen:ident {
        $($variant:ident $(($($item:ident : $ty:ty),+))? $({$($field:ident : $field_ty:ty),+})?),* $(,)?
    } $({fallback $pat:pat => $label:ident})?) => {
        #[derive(Debug)]
        pub enum $frozen {
            $($variant $(($($ty),+))? $({$($field: $field_ty),+})?),*
        }
        impl $frozen {
            fn from_tree(value: $legacy, b: &mut Builder) -> IndexResult<Self> {
                match value {
                    $($legacy::$variant $(($($item),+))? $({$($field),+})? => Ok(
                        Self::$variant $(($($item.freeze(b)?),+))? $({$($field: $field.freeze(b)?),+})?
                    ),)*
                    $($pat => Err(IndexError::SemanticNode(SemanticSyntax::$label)),)?
                }
            }
            fn to_tree(&self, ast: &IndexedAst) -> $legacy {
                match self {
                    $(Self::$variant $(($($item),+))? $({$($field),+})? =>
                        $legacy::$variant $(($(Freeze::thaw($item, ast)),+))? $({$($field: Freeze::thaw($field, ast)),+})?
                    ),*
                }
            }
        }
    };
}

mod body;
mod declarations;
pub use body::*;
pub use declarations::*;

mod sealed {
    pub trait Node {}
    pub trait Element {}
}

/// A node type with a dedicated table. Implementations are supplied by this module.
pub trait Table: sealed::Node + Sized {
    #[doc(hidden)]
    fn nodes(ast: &IndexedAst) -> &[Self];
}

/// An element type with a dedicated list pool.
pub trait ListElement: sealed::Element + Sized {
    #[doc(hidden)]
    fn elements(ast: &IndexedAst) -> &[Self];
}

trait BuildTable: Table {
    fn nodes_mut(b: &mut Builder) -> &mut Vec<Self>;
}
trait BuildList: ListElement {
    fn scratch(b: &mut Builder) -> &mut Vec<Self>;
    fn finish_list(b: &mut Builder, base: usize) -> IndexResult<List<Self>>;
}

macro_rules! tables {
    (nodes { $($node_field:ident: $node:ty),* $(,)? }
     lists { $($list_field:ident: $item:ty),* $(,)? }) => {
        #[derive(Default)]
        struct Builder {
            names_by_spelling: HashMap<Arc<str>, NameId>,
            scratch: ListScratch,
            $($node_field: Vec<$node>,)*
            $($list_field: Vec<$item>,)*
        }

        #[derive(Default)]
        struct ListScratch {
            $($list_field: Vec<$item>,)*
        }

        /// A complete frozen syntax unit. Edges only refer to its own tables.
        #[derive(Debug)]
        pub struct IndexedAst {
            root: Id<ModuleNode>,
            $($node_field: Box<[$node]>,)*
            $($list_field: Box<[$item]>,)*
        }

        impl Builder {
            fn finish(self, root: Id<ModuleNode>) -> IndexedAst {
                IndexedAst {
                    root,
                    $($node_field: self.$node_field.into_boxed_slice(),)*
                    $($list_field: self.$list_field.into_boxed_slice(),)*
                }
            }
        }

        $(impl sealed::Node for $node {}
        impl Table for $node {
            fn nodes(ast: &IndexedAst) -> &[Self] { &ast.$node_field }
        }
        impl BuildTable for $node {
            fn nodes_mut(b: &mut Builder) -> &mut Vec<Self> { &mut b.$node_field }
        })*

        $(impl sealed::Element for $item {}
        impl ListElement for $item {
            fn elements(ast: &IndexedAst) -> &[Self] { &ast.$list_field }
        }
        impl BuildList for $item {
            fn scratch(b: &mut Builder) -> &mut Vec<Self> { &mut b.scratch.$list_field }

            fn finish_list(b: &mut Builder, base: usize) -> IndexResult<List<Self>> {
                let pending = &mut b.scratch.$list_field;
                let pool = &mut b.$list_field;
                let start = index(pool.len())?;
                let count = index(pending.len() - base)?;
                let end = start.checked_add(count).ok_or(IndexError::Capacity)?;
                pool.extend(pending.drain(base..));
                Ok(List { start, end, element: PhantomData })
            }
        })*
    };
}

tables! {
    nodes {
        names: Arc<str>,
        expressions: ExprNode,
        statements: StmtNode,
        patterns: PatternNode,
        types: TypeNode,
        functions: FunctionNode,
        modules: ModuleNode,
        literals: LiteralNode,
    }
    lists {
        names_list: NameId,
        expr_list: Id<ExprNode>,
        stmt_list: Id<StmtNode>,
        pattern_list: Id<PatternNode>,
        types_list: Id<TypeNode>,
        function_list: Id<FunctionNode>,
        expr_fields: (NameId, Id<ExprNode>),
        pattern_fields: (NameId, Id<PatternNode>),
        type_fields: (NameId, Id<TypeNode>),
        source_spellings: (NameId, NameId),
        param_types: (Id<TypeNode>, bool),
        bounds: (i64, i64),
        match_arms: MatchArmNode,
        case_arms: (CaseLabelNode, List<Id<StmtNode>>),
        module_declarations: ModuleDeclarationNode,
        enum_declarations: EnumDeclarationNode,
        variants: VariantDeclarationNode,
        aliases: TypeAliasNode,
        records: RecordNode,
        traits: TraitNode,
        implementations: ImplementationNode,
        inherent_implementations: InherentImplementationNode,
        inherent_methods: InherentMethodNode,
        properties: PropertyNode,
        worlds: WorldNode,
        world_items: WorldItemNode,
        interface_implementations: InterfaceImplementationNode,
        imports: ImportNode,
        annotations: AnnotationNode,
        constraints: ConstraintNode,
        parameters: ParamNode,
        captures: CaptureNode,
    }
}

fn index(n: usize) -> IndexResult<u32> {
    u32::try_from(n).map_err(|_| IndexError::Capacity)
}

impl Builder {
    fn push<T: BuildTable>(&mut self, value: T) -> IndexResult<Id<T>> {
        let nodes = T::nodes_mut(self);
        let next = nodes.len().checked_add(1).ok_or(IndexError::Capacity)?;
        let id = Id(
            NonZeroU32::new(index(next)?).ok_or(IndexError::Capacity)?,
            PhantomData,
        );
        nodes.push(value);
        Ok(id)
    }
}

impl IndexedAst {
    /// Consume the parser's source tree. Linked and lowered nodes are rejected.
    pub fn from_source_tree(tree: Ast) -> Result<Self, IndexError> {
        let mut b = Builder::default();
        let root = tree.freeze(&mut b)?;
        Ok(b.finish(root))
    }

    pub fn root(&self) -> &ModuleNode {
        self.node(self.root)
    }

    /// Look up a node ID obtained from this unit.
    pub fn get<T: Table>(&self, id: Id<T>) -> Option<&T> {
        T::nodes(self).get((id.0.get() - 1) as usize)
    }

    /// Read a list obtained from this unit.
    pub fn get_list<T: ListElement>(&self, list: List<T>) -> Option<&[T]> {
        T::elements(self).get(list.start as usize..list.end as usize)
    }

    pub fn name(&self, id: NameId) -> Option<&str> {
        self.get(id).map(|name| name.as_ref())
    }

    /// Compatibility adapter for consumers that still take a mutable tree.
    pub fn to_source_tree(&self) -> Ast {
        Ast::thaw(&self.root, self)
    }

    /// Materialize the declaration headers while letting a consumer retain
    /// selected root routine bodies in its own representation.
    pub fn to_source_tree_with_bodies(&self, include_body: &impl Fn(Span) -> bool) -> Ast {
        declarations::thaw_module(self.root, self, include_body)
    }

    fn node<T: Table>(&self, id: Id<T>) -> &T {
        self.get(id).expect("syntax node belongs to this unit")
    }
}

macro_rules! freeze_copy {
    ($($ty:ty),+ $(,)?) => {$(
        impl Freeze for $ty {
            type Stored = Self;
            fn freeze(self, _: &mut Builder) -> IndexResult<Self> { Ok(self) }
            fn thaw(v: &Self, _: &IndexedAst) -> Self { *v }
        }
    )+};
}

freeze_copy!(
    Span,
    bool,
    i64,
    f64,
    usize,
    char,
    Number,
    NumberKind,
    MemoryKind,
    Locality,
    ObjectKind,
    RoutineKind,
    FunctionBodyKind,
    RoutineAttributes,
    RoutineOrigin,
    ParameterArity,
    ParameterPassing,
    TraitOrigin,
    ModuleBoundary,
    Operator,
);

impl Freeze for Box<str> {
    type Stored = NameId;
    fn freeze(self, b: &mut Builder) -> IndexResult<NameId> {
        if let Some(&id) = b.names_by_spelling.get(self.as_ref()) {
            return Ok(id);
        }
        let name: Arc<str> = self.into();
        let id = b.push(Arc::clone(&name))?;
        b.names_by_spelling.insert(name, id);
        Ok(id)
    }
    fn thaw(v: &NameId, ast: &IndexedAst) -> Self {
        ast.node(*v).as_ref().into()
    }
}

impl<T: Freeze> Freeze for Box<T> {
    type Stored = T::Stored;
    fn freeze(self, b: &mut Builder) -> IndexResult<Self::Stored> {
        (*self).freeze(b)
    }
    fn thaw(v: &Self::Stored, ast: &IndexedAst) -> Self {
        Box::new(T::thaw(v, ast))
    }
}

impl<T: Freeze> Freeze for Option<T> {
    type Stored = Option<T::Stored>;
    fn freeze(self, b: &mut Builder) -> IndexResult<Self::Stored> {
        self.map(|x| x.freeze(b)).transpose()
    }
    fn thaw(v: &Self::Stored, ast: &IndexedAst) -> Self {
        v.as_ref().map(|x| T::thaw(x, ast))
    }
}

impl<A: Freeze, B: Freeze> Freeze for (A, B) {
    type Stored = (A::Stored, B::Stored);
    fn freeze(self, b: &mut Builder) -> IndexResult<Self::Stored> {
        let (a, c) = self;
        Ok((a.freeze(b)?, c.freeze(b)?))
    }
    fn thaw(v: &Self::Stored, ast: &IndexedAst) -> Self {
        (A::thaw(&v.0, ast), B::thaw(&v.1, ast))
    }
}

impl<T: Freeze> Freeze for Vec<T>
where
    T::Stored: BuildList,
{
    type Stored = List<T::Stored>;
    fn freeze(self, b: &mut Builder) -> IndexResult<Self::Stored> {
        // Nested lists use the scratch suffix beyond their parent's pending
        // elements. Committing a list drains its suffix and keeps the capacity.
        let base = T::Stored::scratch(b).len();
        for value in self {
            let stored = value.freeze(b)?;
            T::Stored::scratch(b).push(stored);
        }
        T::Stored::finish_list(b, base)
    }
    fn thaw(v: &Self::Stored, ast: &IndexedAst) -> Self {
        ast.get_list(*v)
            .expect("syntax list belongs to this unit")
            .iter()
            .map(|v| T::thaw(v, ast))
            .collect()
    }
}
