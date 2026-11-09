# Vendored crates

Boa crates from crates.io, patched in through `[patch.crates-io]` in the
workspace `Cargo.toml`. Changes are marked `Feather:` in the source.

## boa_ast 0.22.0

Two changes, both in `src/scope.rs`.

**Name lookups are indexed.** Every scope kept its bindings in a list that
was scanned for each name looked up or declared. Closure-compiled bundles
declare thousands of names in one function scope, so parsing and compiling
them was quadratic: YouTube's 9.5 MB app bundle took 5.8 s to parse and
6.9 s to compile. A name → position map (used once a scope has more than
eight bindings) brings that to 1.1 s and 0.4 s.

**`this` in nested arrows** (`escape_this_in_enclosing_function_scope`):

`this` used inside an arrow function marks the enclosing function scope as
needing a `this` binding. Upstream marked only the nearest enclosing function
scope, which for `() => () => this` is the outer arrow. Its `this` is lexical
too, so the method's binding was never kept and the inner arrow saw the wrong
`this` (and `this.#private` threw "Private element does not exist on object",
which broke GitHub's React app). The fix marks every enclosing function scope.

## boa_gc 0.22.0

One change, in `src/lib.rs` (`GcConfig::default`): collections start at
16 MB rather than 1 MB, and after one the next is due when the heap reaches
about 3x what survived rather than 1.4x. Each collection marks the whole live
heap, so on large pages collecting that often was a quarter of script time.

Drop these when Boa releases include equivalent fixes.
