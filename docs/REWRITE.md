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
   Cascade correctness found on the way: `@layer` (layer order across a
   document's sheets, nested layers, `!important` reversed; GitHub's
   Primer styles are layered, so unlayered site CSS has to beat them) and
   `var()` that can't be resolved making the property `unset` (invalid at
   computed-value time), not leaving earlier rules' values in place.
3. **Lossless values**: parse into what CSS specifies and resolve late,
   against the box. Grid templates and placements are now kept whole
   (see stage 2). Known losses: lengths with units we don't keep,
   `background-repeat` beyond x/y, `transform` lists beyond
   translate/rotate/scale.
4. **Split `cascade.rs`** (done): `cascade/` has the cascade itself in
   `mod.rs`, and `sources` (UA sheet, document sheets, `@import`,
   presentational hints), `index` (rule indexes and cascade order), `apply`
   (declarations to computed values), `vars`, `size` (estimates for
   container units) and `dark`. Static pages render pixel-identically
   before and after (beyond run-to-run noise).

## Stage 2: layout

Done so far, inside the current renderer:
- App shells split into rows: a flex container whose one in-flow item fills
  it (a column's stretched child, a row's growing or 100%-wide one; empty
  siblings ignored) is laid out like a block, so it splits; auto-height
  clipping boxes split too. YouTube's results went from 2 rows (nothing
  virtualized, nothing to anchor the scroll to) to ~100; a scripted scroll
  recording renders ~4x faster with fewer, smaller jumps.
- The view is kept on the row with the same element across rebuilds.
- `LayoutTree::reuse_unchanged` (keeping unchanged rows across a rebuild)
  took ~160ms per update on YouTube: every box's style carries its
  inherited CSS variables, hundreds there, compared map by map. Equal
  maps are now shared (interned per page), so they compare by pointer:
  under 1ms.
- Real CSS grid: the style crate keeps full track lists (`repeat()`,
  `minmax()`, `fit-content()`, line names, `auto-fill`/`auto-fit`),
  `grid-auto-*`, `grid-template-areas` and every placement form
  (`name`, `2 name`, `span name`, negative lines). GPUI is vendored
  (`vendor/gpui-pre`) with `Style::css_grid`/`css_grid_item`, handed to
  taffy as-is. Grids whose items need it (named lines, rows, line ranges)
  go through it; simple column grids keep the row-splitting path. The
  Guardian's fronts (named-line columns, `grid-column: content`) now lay
  out like Edge; they were stacked into one column before.

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

Some of it isn't DOM and doesn't move to Rust at all: it was the engine's
job done in JavaScript, and goes to the engine.
- Shadow DOM styles (done): shadow trees' styles were rewritten as text
  (`[data-feather-host=N] .x[data-feather-in=N]`). Now the flattened
  document says which tree each node is in (`page::ShadowTrees`), each
  stylesheet has a scope, and selectors match across tree boundaries the
  way the `selectors` crate implements the spec: `:host`, `:host(...)`,
  `::slotted(...)`, descendant combinators stopping at the shadow root,
  context order between trees. The document's rules no longer reach into
  shadow trees (a page's `* { box-sizing: border-box }` doesn't, in any
  browser). `:host-context()` isn't supported (Firefox doesn't either).

## Parser parity

`cargo run --release -p style --example decls -- *.css` prints every rule's
declarations. Run it from the old and new trees over the same stylesheets
and compare the outputs: a refactor that should change nothing must
produce identical output.
