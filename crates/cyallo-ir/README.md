# Cyallo semantic analysis and checked IR

Follow the [compiler and runtime rules](../../docs/compiler-and-runtime-rules.md)
when changing resolution, primitive identities, specialization, or diagnostics.

This crate owns name resolution, generic specialization, type checking, effect
analysis, and translation to backend-independent operations. It does not depend
on a parser, bytecode interpreter, native emitter, or runtime session.

Parsers produce `cyallo-ast` syntax. Resolution binds calls to declarations;
standard declarations carry opaque intrinsic identities. Type analysis
instantiates those declarations against operand types before emitting typed IR.
Backend compilers consume that IR and never interpret source spellings.

Known routine calls carry function indices. Known primitives carry `Intrinsic`
variants and instantiated result types. Indirect and trait calls retain their
checked signatures and dispatch slots. Runtime specialization must establish its
target and receiver assumptions before replacing an indirect operation.
