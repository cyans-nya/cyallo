use super::*;

enum_syntax!(Literal => LiteralNode {
    Number(value: Number),
    Integer(value: i64),
    Real(value: f64),
    Boolean(value: bool),
    Char(value: char),
    String(text: NameId),
});

enum_syntax!(Type => TypeNode {
    Lifetime(name: NameId),
    SelfType,
    Numeric(kind: NumberKind),
    Integer,
    Real,
    Boolean,
    Char,
    String,
    Array(bounds: List<(i64, i64)>, element: Id<TypeNode>),
    SparseArray(element: Id<TypeNode>),
    Named(name: NameId),
    Applied(name: NameId, arguments: List<Id<TypeNode>>),
    AnonymousRecord(fields: List<(NameId, Id<TypeNode>)>),
    Tuple(elements: List<Id<TypeNode>>),
    Function(kind: RoutineKind, parameters: List<(Id<TypeNode>, bool)>, result: Id<TypeNode>),
    Reference { mutable: bool, lifetime: Option<NameId>, pointee: Id<TypeNode> },
    Managed { kind: MemoryKind, locality: Locality, lifetime: Option<NameId>, pointee: Id<TypeNode> },
    Slice(element: Id<TypeNode>),
    TraitObject(name: NameId),
    Any,
    Dynamic,
    Object(fields: List<(NameId, Id<TypeNode>)>),
    Void,
} {fallback Type::Never
    | Type::NativeCallable(..) | Type::ValueParameter(..) | Type::Dependent(..)
    | Type::Record(..) | Type::RecordRef(..) | Type::Enum(..) | Type::EnumRef(..)
    | Type::Borrowed(..) | Type::Context | Type::Registration | Type::ServiceKey(..)
    | Type::ServiceRef(..) | Type::Storage(..) | Type::Opaque(..) | Type::Symbol => Type});

enum_syntax!(ExprKind => ExprSyntax {
    Yield(value: Id<ExprNode>, delegate: bool),
    ManagedNew(kind: MemoryKind, locality: Locality, allocator: Option<Id<ExprNode>>, value: Id<ExprNode>),
    Literal(value: Id<LiteralNode>),
    Name(name: NameId),
    ObjectLiteral(kind: ObjectKind, fields: List<(NameId, Id<ExprNode>)>),
    AnonymousRecord(fields: List<(NameId, Id<ExprNode>)>),
    Match(value: Id<ExprNode>, arms: List<MatchArmNode>),
    DeleteProperty(value: Id<ExprNode>),
    ArrayLiteral(values: List<Id<ExprNode>>),
    Tuple(values: List<Id<ExprNode>>),
    Record(name: NameId, fields: List<(NameId, Id<ExprNode>)>),
    Field(base: Id<ExprNode>, name: NameId),
    TupleField(base: Id<ExprNode>, index: usize),
    Invoke(callee: Id<ExprNode>, args: List<Id<ExprNode>>),
    Closure(function: Id<FunctionNode>, captures: List<CaptureNode>),
    TypedCollection(name: NameId, args: List<Id<TypeNode>>, values: List<Id<ExprNode>>),
    GenericCall(name: NameId, args: List<Id<TypeNode>>, values: List<Id<ExprNode>>),
    GenericRecord(name: NameId, args: List<Id<TypeNode>>, fields: List<(NameId, Id<ExprNode>)>),
    Cast(value: Id<ExprNode>, ty: Id<TypeNode>),
    Parenthesized(value: Id<ExprNode>),
    Index(base: Id<ExprNode>, args: List<Id<ExprNode>>),
    Call(name: NameId, args: List<Id<ExprNode>>),
    Unary(operator: Operator, value: Id<ExprNode>),
    Binary(operator: Operator, left: Id<ExprNode>, right: Id<ExprNode>),
} {fallback ExprKind::NativeCoerce(..) | ExprKind::Invalid(..) | ExprKind::IntrinsicCall(..)
    | ExprKind::Frozen(..) | ExprKind::TypeDescriptor(..) | ExprKind::DeferredCallable(..)
    | ExprKind::VariantPayload(..) | ExprKind::EnumConstruct(..) | ExprKind::ClosureValue(..)
    | ExprKind::ExtensionField(..) => Expression});

enum_syntax!(PatternKind => PatternSyntax {
    Wildcard,
    Binding(name: NameId),
    Variant(name: NameId, payload: Option<Id<PatternNode>>),
    Tuple(patterns: List<Id<PatternNode>>),
    Record(fields: List<(NameId, Id<PatternNode>)>, rest: bool),
    Literal(value: Id<LiteralNode>),
    Or(patterns: List<Id<PatternNode>>),
} {fallback PatternKind::ResolvedVariant(..) => VariantPattern});

enum_syntax!(StmtKind => StmtSyntax {
    Binding(name: NameId, ty: Option<Id<TypeNode>>, constant: bool, mutable: bool, value: Id<ExprNode>),
    Compound(target: Id<ExprNode>, operator: Operator, value: Id<ExprNode>),
    Evaluate(value: Id<ExprNode>),
    Break,
    Continue,
    ReturnVoid,
    Declare(name: NameId, ty: Id<TypeNode>),
    Constant(name: NameId, value: Id<ExprNode>),
    Assign(target: Id<ExprNode>, value: Id<ExprNode>),
    Output(values: List<Id<ExprNode>>),
    Input(value: Id<ExprNode>),
    If(condition: Id<ExprNode>, yes: List<Id<StmtNode>>, no: List<Id<StmtNode>>),
    IfLet(pattern: Id<PatternNode>, value: Id<ExprNode>, yes: List<Id<StmtNode>>, no: List<Id<StmtNode>>),
    While(condition: Id<ExprNode>, body: List<Id<StmtNode>>),
    Repeat(body: List<Id<StmtNode>>, condition: Id<ExprNode>),
    For(name: NameId, first: Id<ExprNode>, last: Id<ExprNode>, step: Option<Id<ExprNode>>, body: List<Id<StmtNode>>),
    ForIn(name: NameId, value: Id<ExprNode>, body: List<Id<StmtNode>>),
    Case(value: Id<ExprNode>, arms: List<(CaseLabelNode, List<Id<StmtNode>>)>, fallback: List<Id<StmtNode>>),
    Call(name: NameId, args: List<Id<ExprNode>>),
    Return(value: Id<ExprNode>),
});

enum_syntax!(MatchBody => MatchBodyNode {
    Expression(value: Id<ExprNode>),
    Block(body: List<Id<StmtNode>>, tail: Option<Id<ExprNode>>),
});

impl Freeze for MatchBody {
    type Stored = MatchBodyNode;
    fn freeze(self, b: &mut Builder) -> IndexResult<Self::Stored> {
        MatchBodyNode::from_tree(self, b)
    }
    fn thaw(v: &Self::Stored, ast: &IndexedAst) -> Self {
        v.to_tree(ast)
    }
}

#[derive(Debug)]
pub struct MatchArmNode {
    pub pattern: Id<PatternNode>,
    pub guard: Option<Id<ExprNode>>,
    pub body: MatchBodyNode,
    pub span: Span,
}

impl Freeze for MatchArm {
    type Stored = MatchArmNode;
    fn freeze(self, b: &mut Builder) -> IndexResult<Self::Stored> {
        let Self {
            package,
            pattern,
            guard,
            body,
            span,
        } = self;
        if package.is_some() {
            return Err(IndexError::SemanticNode(SemanticSyntax::MatchArm));
        }
        Ok(MatchArmNode {
            pattern: pattern.freeze(b)?,
            guard: guard.freeze(b)?,
            body: body.freeze(b)?,
            span,
        })
    }
    fn thaw(v: &Self::Stored, ast: &IndexedAst) -> Self {
        Self {
            package: None,
            pattern: Freeze::thaw(&v.pattern, ast),
            guard: Freeze::thaw(&v.guard, ast),
            body: Freeze::thaw(&v.body, ast),
            span: v.span,
        }
    }
}

struct_syntax!(CaseLabel => CaseLabelNode {
    lower: Id<ExprNode>,
    upper: Option<Id<ExprNode>>,
});

macro_rules! store {
    ($($tree:ident => $node:ident),+ $(,)?) => {$(
        impl Freeze for $tree {
            type Stored = Id<$node>;
            fn freeze(self, b: &mut Builder) -> IndexResult<Self::Stored> {
                with_stack(|| {
                    let node = $node::from_tree(self, b)?;
                    b.push(node)
                })
            }
            fn thaw(v: &Self::Stored, ast: &IndexedAst) -> Self {
                with_stack(|| ast.node(*v).to_tree(ast))
            }
        }
    )+};
}

macro_rules! spanned {
    ($($tree:ident => $node:ident : $kind:ident),+ $(,)?) => {$(
        #[derive(Debug)]
        pub struct $node {
            pub kind: $kind,
            pub span: Span,
        }
        impl $node {
            fn from_tree(value: $tree, b: &mut Builder) -> IndexResult<Self> {
                let $tree { kind, span } = value;
                Ok(Self { kind: $kind::from_tree(kind, b)?, span })
            }
            fn to_tree(&self, ast: &IndexedAst) -> $tree {
                $tree { kind: self.kind.to_tree(ast), span: self.span }
            }
        }
    )+};
}

spanned!(Expr => ExprNode: ExprSyntax, Stmt => StmtNode: StmtSyntax, Pattern => PatternNode: PatternSyntax);
store!(Expr => ExprNode, Stmt => StmtNode, Pattern => PatternNode, Type => TypeNode, Literal => LiteralNode);
