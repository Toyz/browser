# Vendored crates

## boa_ast 0.22.0

Boa's AST crate from crates.io, patched in through `[patch.crates-io]` in the
workspace `Cargo.toml`. One change, in `src/scope.rs`
(`escape_this_in_enclosing_function_scope`):

`this` used inside an arrow function marks the enclosing function scope as
needing a `this` binding. Upstream marked only the nearest enclosing function
scope, which for `() => () => this` is the outer arrow. Its `this` is lexical
too, so the method's binding was never kept and the inner arrow saw the wrong
`this` (and `this.#private` threw "Private element does not exist on object",
which broke GitHub's React app). The fix marks every enclosing function scope.

Drop this when a Boa release includes the fix.
