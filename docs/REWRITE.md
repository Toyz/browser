# Engine rewrite

Feather renders many sites, but too much of it is approximation: values are
thrown away while parsing, layout is CSS translated into GPUI divs with a
workaround per feature, and the DOM is a 3,400-line JavaScript file. Each
site fix has been a patch over one of those. This is the plan to replace
them, in stages that each leave the browser working.

Every stage is verified the same way: the test suite passes, and pages
render no worse than before (offscreen screenshots of a fixed set of
sites, compared against headless Edge). Refactors that shouldn't change
behavior are checked for exact equivalence (see "parser parity" below).

## Stage 1: style

1. **Property registry** (done): `props/` is split by area (`box_model`,
   `text`, `background`, `flex_grid`, `effects`, `animation`); each
   property is a named parse function in a table, looked up by name.
   Margins, padding and borders are explicit families. Verified by
   parser parity on 39 real stylesheets (36,919 rules, byte-identical).
   The shared value parsers are split the same way (`length`, `color`,
   `image`, `layout`, `text_values`, `timing`, `border`, with `decl` for
   the declaration types): `props/mod.rs` went from 2,057 lines to 235.
2. **Property metadata** (done): `props/meta.rs` lists, per property,
   whether it inherits and which computed fields it owns; the cascade's
   `inherit`/`initial`/`unset` handling is a lookup in it, and a test checks
   `ComputedStyle::inherit` copies exactly the inherited ones.
3. **Lossless values**: parse into what CSS specifies and resolve late,
   against the box. Known losses: grid track lists (`max()`, `calc()`,
   `minmax(auto, …)` become `Auto`), lengths with units we don't keep,
   `background-repeat` beyond x/y, `transform` lists beyond
   translate/rotate/scale.
4. **Split `cascade.rs`**: matching, the cascade proper, value
   computation (`apply`), and size estimates are separate concerns in one
   1,900-line file.

## Stage 2: layout

GPUI's layout (taffy underneath) supports only part of CSS: grids are N
equal columns, there are no floats, absolute boxes position against their
parent. `render/boxes.rs` works around each gap (template grids rebuilt as
flex rows, floats as two-column rows, absolute boxes hoisted, percentage
transforms applied after layout).

1. Build a CSS box tree from the styled DOM (what `boxes.rs` does, minus
   the workarounds).
2. Lay it out with `taffy` directly (full CSS grid with track lists and
   named areas, flexbox, block), measuring text through GPUI's text
   system, and position absolute boxes against their containing block.
3. Paint the laid-out tree with GPUI, keeping the row virtualization that
   lets long pages scroll cheaply.
4. Delete the workarounds one feature at a time as the new path covers
   them.

## Stage 3: the DOM

`js/src/prelude.js` implements the DOM in JavaScript over a few dozen
native functions. Move the node classes and their hot paths into Rust
(Boa native classes), organized by area (nodes and tree, elements and
attributes, events, observers, CSSOM, network), leaving JavaScript only
where it's the natural place (small shims, polyfill-shaped APIs).

## Parser parity

`cargo run --release -p style --example decls -- *.css` prints every rule's
declarations. Run it from the old and new trees over the same stylesheets
and compare the outputs: a refactor that should change nothing must
produce identical output.
