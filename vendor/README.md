# Vendored crates

Boa crates from crates.io, patched in through `[patch.crates-io]` in the
workspace `Cargo.toml`. Changes are marked `Feather:` in the source.

## boa_ast 0.22.0

Three changes.

**Large node variants are boxed** (`src/expression/mod.rs`,
`src/statement/mod.rs`, `src/declaration/mod.rs`). Every `Expression` was as
big as its biggest variant (a function expression, 192 bytes), every
`Statement` as big as a `try` (464) and every `Declaration` as big as a
function declaration (168), so the common small nodes (identifiers, calls,
member accesses) each paid for the rare large ones. The function and
tagged-template expressions, the `try`/`for-in`/`for-of`/`switch` statements
and the function declarations are now `Box`ed: `Expression` is 56 bytes,
`Statement` 80 and `Declaration` 24. YouTube's 10 MB app bundle parses to a
189 MB tree instead of 436 MB (and ~20% faster), and the page's peak memory
drops from ~1490 MB to ~1253 MB. Measure with
`cargo run --release -p js --example compile_mem -- --sizes` (or a script).

The other two are in `src/scope.rs`.

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

## boa_parser 0.22.0

Vendored only to follow the boxed `boa_ast` variants above: the places that
build them directly wrap the node in `Box::new` (or go through `From`).

## boa_engine 0.22.0

- `src/builtins/array_buffer/mod.rs`, `src/object/builtins/jsarraybuffer.rs`:
  an `ArrayBuffer` can view bytes it doesn't own (`ExternalBytes`,
  `JsArrayBuffer::external`): a WebAssembly memory's `buffer` is the
  memory itself, not a copy. Such buffers are fixed-length, detachable
  only with their detach key (when the memory grows), and can't be
  transferred or resized.
- `src/builtins/intl/number_format/`: `style: "currency"` threw
  "unimplemented" (so did creating such a formatter: GitHub's pull request
  list failed to load), and `style: "percent"` printed the bare number.
  Both are formatted as English writes them: currencies with their ISO
  4217 digits and symbol, code or name (`$1,234.50`, `-$5.00`, `EUR 2.00`,
  `3.00 US dollars`), percentages scaled by 100 with `%`.
- `src/bytecompiler/expression/mod.rs`: compiles the boxed function
  expressions (`&**function`).
- `src/vm/inline_cache/mod.rs`: an inline cache's entries are allocated
  when the first one is cached (`Option<Box<ArrayVec<..>>>`), not inline.
  Every property access in every compiled function has a cache, and most of
  a large bundle never runs: inline, they were 84 MB of YouTube's heap.
- `src/builtins/intl/locale/utils.rs` (`default_locale`): the system locale
  is asked for once per thread. Each `Number.prototype.toLocaleString`
  makes a NumberFormat that asked the OS (`CFLocaleCopyPreferredLanguages`)
  again; DuckDuckGo's results script looped on it for seconds and the page
  stayed blank.
- `src/builtins/number/mod.rs` (`to_locale_string`): with no locales or
  options, the default NumberFormat is made once per context (host data)
  rather than per call.
- `src/vm/inline_cache/mod.rs`: an entry for a property found on the
  prototype also records the prototype's shape, and only hits while it's
  unchanged. A prototype that loses a property is reshaped with its slots
  renumbered, but the cache is keyed by the receiver's shape, which stays:
  the cache read whatever moved into the old slot. (DuckDuckGo: after
  polyfills reworked `Array.prototype`, webpack's `arr.splice(i, 1)` called
  `toLocaleString` in an endless loop.)
- `src/vm/opcode/set/property.rs` (`set_by_name`): sets of an array's
  `length` aren't cached. A cached set writes the slot directly, skipping
  ArraySetLength's deletion of the elements past the new length: the second
  `arr.length = 0; arr[0] = x` left the array "empty" with an element in it
  (Twitch's Apollo cache threw "unknown value").

## boa_gc 0.22.0

One change, in `src/lib.rs` (`GcConfig::default`): collections start at
16 MB rather than 1 MB, and after one the next is due when the heap reaches
about 3x what survived rather than 1.4x. Each collection marks the whole live
heap, so on large pages collecting that often was a quarter of script time.

Drop these when Boa releases include equivalent fixes.

## taffy 0.15.1

- `compute/flexbox.rs` (`resolve_cross_axis_auto_margins`): an item larger
  than its flex line gets a zero start auto margin instead of a negative
  centered one (css-flexbox §9.6 step 13; also what block layout does with
  `margin: 0 auto` on an overflowing box).

## gpui-pre 0.3.11

- `src/style.rs`, `src/taffy.rs`: full CSS grids. `Style::css_grid` carries a
  container's template (any track sizes, `repeat()` including `auto-fill` /
  `auto-fit`, named lines, `grid-template-areas`, auto flow) and
  `Style::css_grid_item` an item's placement (line numbers, names, spans,
  `justify-self`); both are converted to taffy's grid types when laid out.
  GPUI's own grid fields only know equal columns and spans: the Guardian's
  front page collapsed into one column.
- `src/style.rs`, `src/taffy.rs`: `Style::content_box`, sizes as CSS
  content-box sizes (taffy's `box-sizing: content-box`), so percentage widths
  on padded boxes come out as in browsers.
- `src/window.rs`: a paint-time scale (`Window::with_paint_scale`). Every
  primitive painted inside it (quads, shadows, glyphs, emoji, SVGs, images,
  underlines) and every content mask pushed inside it is mapped about a
  pivot; glyphs are rasterized at the scaled size. Layout and hit testing
  are unchanged. It's CSS `transform: scale()`: GPUI could transform only
  monochrome SVGs.
- `src/elements/list.rs`: `ListState::item_extent` (where an item starts
  and how tall it is, measured or estimated) and `offset_for_y` (the item
  and offset at a pixel position), so scripts can be told where off-screen
  elements are and `window.scrollTo` can scroll past rows not yet measured.
- `src/elements/img.rs`: animated images keep playing while the window
  isn't the focused one (GPUI paused them, which browsers don't).

## gpui-pre-apple 0.3.11

- `src/text_system.rs`: variable fonts draw the requested weight. When the
  best face for a font has a `wght` axis and the weight asked for isn't its
  default, `weight_variant` makes (once) an instance at that weight through
  a CoreText variation descriptor and registers it as a font of its own;
  `id_for_native_font` tells instances apart by their `wght`. Without it,
  every weight of a variable web font (Inter, Roboto Flex...) drew as its
  default one: NASA's 700-weight headline was regular.
- `src/text_system.rs` (`add_fonts`): fonts CoreText can't open or name are
  skipped. font-kit's `family_name` aborts the app on a web font without a
  family name.

## gpui-component 0.9.0

- `src/input/input.rs`, `src/input/textarea.rs`: `caret_color()` on `Input`
  and `Textarea`, used instead of the theme's caret when projecting the
  editor style. Fields in web pages are drawn in the page's colors; with the
  app in dark mode the theme's caret was white, invisible in a white field.
