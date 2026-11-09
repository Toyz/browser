# Native GPUI Browser — Plan

A web browser where pages render natively: HTML is turned into GPUI elements instead of being painted by a separate web engine. The "DOM" is just our own lightweight page model. GPUI is the view, and later JS gets a DOM-shaped API over that model.

**Goal:** a fast, native, good-looking browser for the *document* web (blogs, docs, Wikipedia, HN, forums), with JS that works well enough for common sites, plus graceful degradation everywhere else.

**Non-goals (for now):** canvas, WebGL, iframes, web components / shadow DOM, extensions, full spec compliance, the modern app web (Figma, Google Docs).

---

## Architecture

```
            ┌──────────┐   bytes   ┌──────────┐  tokens  ┌────────────┐
 URL bar ──▶│   net    │──────────▶│   html   │─────────▶│ page model │◀──── js (later)
            └──────────┘           └──────────┘          │ (our "DOM")│      DOM-shaped API
                                                         └─────┬──────┘
                                                               │ nodes + stylesheets
                                                         ┌─────▼──────┐
                                                         │   style    │ selectors, cascade,
                                                         │            │ inheritance → ComputedStyle
                                                         └─────┬──────┘
                                                         ┌─────▼──────┐
                                                         │  render    │ page model → GPUI elements
                                                         │            │ (div / StyledText / img)
                                                         └─────┬──────┘
                                                         ┌─────▼──────┐
                                                         │   shell    │ window, tabs, URL bar,
                                                         │  (GPUI)    │ history, scrolling
                                                         └────────────┘
```

Cargo workspace, one crate per box:

| Crate   | Job | Key deps |
|---------|-----|----------|
| `shell` | GPUI app: window, URL bar, back/forward, scroll, tabs | `gpui-kit` (GPUI + gpui-component) |
| `net`   | Fetch, redirects, URL resolution, simple cache | `ureq`, `url` |
| `page`  | Arena tree: `NodeId` → `{ kind, tag, attrs, children, parent }`. Stable IDs (which JS will need later) | `slotmap` |
| `html`  | Spec-compliant parsing straight into `page` (implement html5ever's `TreeSink`) | `html5ever` |
| `style` | CSS parsing, selector matching, cascade, inheritance, UA stylesheet | `cssparser`, `selectors` |
| `render`| Turns page model + computed styles into a GPUI element tree | `gpui-kit` |
| `js`    | (Phase 4) Embedded engine + DOM bindings over `page` | `rquickjs` |

### Key design decisions

1. **Our own tree, not GPUI's.** GPUI elements are rebuilt every frame and then thrown away. The page model is the long-lived state, and `render()` builds elements from it. This is exactly how GPUI apps already work (an entity holds state and `cx.notify()` triggers a re-render), so JS changing the page later is just another state change.
2. **Block layout → GPUI `div`s.** GPUI uses Taffy under the hood, so flex and grid map almost directly. Normal block flow is treated as a vertical flex column (`flex_col`), which looks right in most cases.
3. **Inline content → one `StyledText` per paragraph.** This is the main trick. A run of inline content (`<p>Hello <b>bold</b> <a>link</a></p>`) gets flattened into a single text with styled runs (font, weight, color, underline). Links become clickable ranges via `InteractiveText`. That gives us real text wrapping without writing an inline layout engine. Limitation: boxes inside text (an inline image, an inline-block) need special handling or are moved to their own line.
4. **Use Servo's `selectors` crate, but our own cascade.** Selector matching is fiddly and `selectors` works with any tree through a trait. The cascade and computed styles cover only the properties we support, which keeps it small. (Swapping in Stylo is possible later but much heavier.)
5. **Unknown CSS is ignored, unknown JS APIs are stubs.** Never crash, always degrade.

### Prior art to read
- **Blitz** (DioxusLabs/blitz): *not* the same idea. It's a small browser engine without a webview. It keeps its own DOM, styles it with Stylo, lays it out with Taffy, sets text with Parley, and paints onto a canvas through Vello, so the page is one drawn surface, not UI elements. Useful only as a reference for wiring html5ever, Stylo and Taffy together. **What makes this project different:** the page's output *is* a GPUI element tree, so text, focus, inputs, selection and theming come from GPUI itself.
- **Zed's markdown preview**: markdown → GPUI elements, the same idea on a small scale.
- **linkedom / happy-dom**: what a "good enough" JS DOM looks like.

---

## Phases

### Phase 0 — Skeleton
- [x] Cargo workspace on `gpui-kit` 0.9 (re-exports GPUI as `gpui-pre =0.3.11`, plus `gpui-component` widgets; never list `gpui` directly)
- [x] GPUI window with a URL bar (`gpui-component` `Input`) and a scrollable content area
- [x] Fetch a URL (`ureq` on the background executor), show raw text in the content area

**Done when:** you type a URL and see its source text.

### Phase 1 — HTML → native page (no author CSS)
- [x] `TreeSink` for html5ever → `page` arena (scripting reported off, so `<noscript>` renders)
- [x] Hardcoded UA stylesheet: `h1–h6`, `p`, `ul/ol/li`, `a`, `b/strong`, `i/em`, `code/pre`, `blockquote`, `hr`, `br`, plus `bgcolor` / `<font color>` / `align` hints
- [x] Block vs inline split; paragraphs → `StyledText` (links via `InteractiveText`)
- [x] Clickable links → navigation; back/forward history; `#fragment` scrolling; hover shows URL
- [x] Skip `<script>`, `<style>`, `<head>` contents
- [x] Virtualized rendering: transparent blocks flatten into top-level rows in a GPUI `list()` (with margin collapsing)
- [x] `cargo run --example dump -- <url>` prints layout rows + timings (Wikipedia's Rust article: parse ~130ms, layout ~20ms)

Known Phase 1 limits: one font size per paragraph (inline `<small>`/`<sup>` use the paragraph's size); tables are flex rows (no shared column widths); no images; non-UTF-8 pages decode lossily.

**Done when:** Wikipedia and Hacker News are readable and you can click around.

### Phase 2 — Real CSS
- [x] Collect `<style>` and `<link rel=stylesheet>` (fetched in parallel), `@import`, parse with `cssparser` 0.38
- [x] Selector matching (`selectors` 0.42, incl. `:is/:where/:not/:has`, nth-child), rules bucketed by id/class/tag, ancestor bloom filter; cascade by origin → specificity → order, `!important`, `style=""`, presentational hints
- [x] Custom properties + `var()` with fallbacks; `inherit/initial/unset`
- [x] Properties: display (incl. flex/grid/contents), position (absolute/fixed hidden for now), colors (hex/rgb/hsl/hwb/lab/lch/oklab/oklch/`light-dark()`/system colors), backgrounds (gradients → first stop), fonts + `font` shorthand, line-height, text-align/decoration/transform, white-space modes, visibility, list-style, margin/padding (+ logical), borders (+ shorthands), border-radius, width/height/min/max, overflow, opacity, flexbox, grid (column count, spans), gap
- [x] Units: px/pt/em/rem/ex/ch/%/vw/vh/vmin/vmax, `calc()`, `min()/max()/clamp()`
- [x] Media queries: types, width/height (incl. range syntax), `prefers-color-scheme`, `and/or/not`
- [x] UA stylesheet is real CSS ([ua.css](crates/style/src/ua.css))
- [x] Rendering: flex/grid containers, sized & centered boxes, borders/radius/backgrounds; splittable containers become per-row *wrappers* so centered/background layouts stay virtualized
- [x] Engine crates built with opt-level 2 in dev (Wikipedia: style + layout ≈ 35ms)
- [x] `::before`/`::after` generated content (strings, `attr()`, quotes; icon-font glyphs dropped)
- [x] Floats, approximated: a floated box + the following content (until a heading/`clear`) become a two-column row
- [x] Real fonts: `font-family` lists resolve to the first *installed* family (Verdana on HN, Georgia on Wikipedia)
- [x] Min-content width estimates (GPUI measures text min-width as the whole line), so flex items/cells shrink like CSS
- [x] `calc()` in media queries; CSS block semantics for auto-margin boxes
- [x] Interactive states: `:hover` (blocks report via GPUI hover, inline elements via the inline engine's hit-testing; the chain includes flattened ancestors), `:active`, `:target`, `:focus*` matching (nothing focusable yet). Restyles run in the background only when the page has state rules. CSS `cursor`.
- [x] Faster state restyles: only elements whose state changed are restyled; paint-only changes are patched into the layout (no re-layout)
- [x] `transition` / `@keyframes` animations: interpolate paint properties (colors, opacity, transforms) per frame on top of the hover patcher, no re-layout (a compositor-style fast path)
- [x] Dark mode (`prefers-color-scheme: dark` + a dark canvas)
- [x] Re-style on window resize: rules keep their `@media` conditions; pages keep `Document` + `StyleData`; when a condition flips, restyle + re-layout in the background (debounced) and swap, keeping scroll position. (Viewport units only update on breakpoint changes for now.)
- [x] `::before/::after` content, fonts from `@font-face` (variable fonts instanced per weight), rotate/scale transforms, real gradients
- [ ] CSS nesting

**Done when:** a typical blog (or a docs site) looks recognizably like it does in Chrome.

### Phase 3 — Polish the document web
- [x] Images: `<img>` with `srcset` (2x), background loading (8 at a time), per-row re-measure; `data:` URLs
- [x] CSS `background-image: url()` (size/position) and `mask-image` icons (GPUI `svg()` tinted with the box color, served via a page-asset source)
- [x] Inline margins/padding approximated with en/thin spaces
- [x] Table layout: column-shared sizing (basis = column max-content, grow ∝ it, min = column min-content), `colspan`
- [x] Quirks mode (no doctype): tables reset `text-align`/fonts (HN's `<center>`)
- [x] **Inline layout engine** ([inline.rs](crates/render/src/inline.rs)): our own GPUI element. Words measured with GPUI's shaper, greedy line breaking, CSS line boxes (per-run font sizes, line-height, `vertical-align`), inline-box decorations (`<code>` chips, `<mark>`), images in the line, real min/max-content answers, link hit-testing.
- [ ] Inline layout, still missing: text wrapping *under* floats, `inline-flex`/`inline-grid` in the line, `text-indent`, justify, breaking overlong words, text selection
- [ ] Tables (approximate with grid)
- [x] Forms: text inputs (gpui-kit editors in CSS-styled boxes), textareas, checkboxes, radios, selects, buttons, labels; GET and POST submission; `:checked`/`:focus`/`:disabled`...
- [x] Source view: CSS/JS/JSON responses and `view-source:` in a highlighted code editor, minified code pretty-printed
- [ ] Find in page, text selection & copy, zoom
- [x] `position: relative` (offsets), `absolute` (relative to the parent box, painted on top via GPUI `deferred()`), `fixed` (viewport overlay layer). Absolute boxes with no positioned ancestor (besides html/body) stay hidden; `sticky` is static for now.
- [x] Real `inline-block`: with inline content, atomic objects laid out as nested inline flows; with block content, real GPUI subtrees laid out as absolute children of the inline element, measured with last frame's sizes and moved into their line slot (one extra frame to settle). Shrink-to-fit, margins, borders, radius, `vertical-align: top/middle/bottom`
- [ ] `position: sticky`; absolute boxes anchored to a non-parent ancestor; `z-index`; inline `position: relative` offsets

**Done when:** you'd actually use it for reading.

### Phase 3.5 — Native browser frame
- [x] Custom title bar with tabs in the window frame (Chrome-style), using gpui-component's `TitleBar`
- [x] Tab strip: new/close (Cmd-T/W, middle click), per-tab history and loading state, favicons, Cmd-click opens links in background tabs
- [ ] Tab reordering by drag
- [x] Omnibox: history suggestions, inline completion, DuckDuckGo search, lock / not-secure / file icons
- [x] Dark mode: the chrome follows the system; pages get `prefers-color-scheme`, and those opting in with `color-scheme` get dark defaults and `light-dark()`
- [x] `file://` pages and directory listings (web pages can't load local files)
- [x] New tab page with most-visited sites; history saved to disk

**Done when:** it looks and feels like a real browser window, not an app with a URL field.

### Phase 4 — JS "works-ish"
- [x] Embed **[Boa](https://github.com/boa-dev/boa)** (a JS engine written in pure Rust — so even the JavaScript is Rust); run `<script>` tags in order, `defer`/`async`
- [x] `console`, `setTimeout/setInterval`, `requestAnimationFrame`, microtasks, an event loop (on a script thread per page; DOM snapshots go to the tab)
- [x] DOM read API: `document`, `getElementById`, `querySelector(All)`, `textContent`, `getAttribute`, `children`, `parentNode`
- [x] DOM write API (the framework subset): `createElement`, `createTextNode`, `appendChild`, `insertBefore`, `removeChild`, `setAttribute`, `classList`, `style.*`, `innerHTML` (setter reuses the `html` crate)
- [x] Events: `addEventListener`, capture/bubbling, `click` (with `preventDefault` on links and buttons), `preventDefault`
- [ ] Events: `input`, `submit`, `keydown` from the page's widgets
- [x] Changes mark the page dirty → restyle → `cx.notify()`
- [ ] `fetch` / `XMLHttpRequest` with CORS, `history.pushState` (needed for SPAs); `localStorage` and `location` are in
- [ ] ES modules, custom elements, shadow DOM (`<slot>`, constructable stylesheets)
- [ ] Stub everything else (no-ops / plausible defaults)

**Done when:** a Preact or React todo app works, and a common SPA's first screen renders.

### Phase 5 — The gnarly bits
- [ ] Layout queries (`getBoundingClientRect`, `offsetWidth`, `scrollTop`): force a layout pass on demand and read results back from GPUI
- [ ] `MutationObserver`, `IntersectionObserver` (lazy-loading images use it)
- [ ] Cookies and login flows
- [x] Performance pass 1 (measured with `--bench`): engine/render crates optimized in dev builds; inline layouts + shaped text cached across frames in element state; shaped lines behind `Rc`; restyles reuse unchanged rows (keeping caches); hover restyles re-measure only visible rows; absolute children no longer block row splitting; in-memory subresource cache; back/forward page cache with scroll restore; loading bar. Wikipedia scroll: 39ms → ~1.3ms/frame.
- [x] Performance pass 2: grids and flex columns split into rows (blizztrack's home page: 1 → 148 rows); links prerender on hover; text shaping cached by content across pages and tabs; images shared between consecutive pages show up instantly; hover restyles wait for scrolling to stop and only re-measure rows that changed
- [x] Hover patching: state changes restyle only the elements involved and patch paint-only changes into the layout (~0.5ms)
- [ ] Performance: incremental restyle for layout-affecting changes, web fonts off the critical path

---

## Dev tools

- `cargo run --example dump -- <url> [<url>...] [rows]` — layout rows + timings (several URLs share the subresource cache). `TREE=1` prints big blocks' structure; `WRAPPERS=1` row wrappers; `FIND=text` a paragraph's styles.
- `--bench FRAMES` (screenshot mode) — steps a row per frame, prints avg/p95/max frame time.
- `cargo run --features screenshot -- <url> --screenshot out.png [--pages N] [--row N] [--scroll PX] [--size WxH]` — renders offscreen via GPUI's `render_to_image` (no screen-recording permission), saves PNGs, quits. Prefer `--row` (from `dump`) over `--scroll`: pixel scrolling skips rows the list hasn't measured. `--resize WxH` resizes after load (tests live restyling). `--mouse X,Y` hovers a point before capturing (tests `:hover`). `BROWSER_DEBUG=1` logs image loads.
- Sites that rate-limit (HN): mirror locally (`curl` the HTML/CSS/images into a folder, `python3 -m http.server`) and test against `http://127.0.0.1:<port>/`.
- Reference rendering: `"/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge" --headless=new --window-size=1100,1600 --screenshot=ref.png <url>`. Note Edge runs JS (Wikipedia renders its `client-js` layout); we're `client-nojs`.

## Risks / open questions

- **GPUI stability and docs.** It's built for Zed first. Expect API churn; pin a version.
- **Inline layout limits.** The `StyledText` trick breaks for inline boxes (inline images, `inline-block`, inline padding and borders). Decide later whether to accept this or write a small inline layout pass.
- **Block flow ≠ flex column.** Margin collapsing and floats don't map onto it. Floats are probably ignored at first (moved to their own line instead).
- **Layout reads from JS.** GPUI does layout during its frame, but JS wants answers synchronously. Needs investigation in Phase 5.
- **Security.** Untrusted JS runs in our process. Boa has no file system or network access unless we give it some, but cap memory and CPU time per page and never expose host APIs carelessly.
- **Build vs reuse.** Don't adopt `blitz-dom`: it owns layout and painting, which defeats the point. The only real choice is styling: our own small cascade (lighter, and we control which properties exist) vs. Stylo (complete, but heavy and built around Servo's DOM traits). Start with our own and revisit if CSS coverage becomes the bottleneck.
