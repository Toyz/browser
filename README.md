# browser

**A web browser where the web page is not a web page.**

Normal browsers parse HTML into a DOM, run it through a rendering engine, and paint the result into a rectangle. This one parses HTML, runs its CSS through a cascade we wrote ourselves, and then turns the whole page into **native [GPUI](https://www.gpui.rs) UI elements**. GPUI is the GPU UI framework behind the Zed editor. Every paragraph you see is a GPUI element. Every `<div>` is a GPUI `div()`. The DOM is a slotmap the renderer reads once and then mostly ignores.

It is cursed. It also kind of works.

![Wikipedia, rendered as native GPUI elements](docs/wikipedia.png)

![Hacker News](docs/hacker-news.png)

## Just how cursed is it

- **Blocks are flexboxes.** CSS has a whole layout mode called "block flow". We don't have it. Every block is a GPUI `flex_col()`, and we fake margin collapsing by doing the arithmetic ourselves before Taffy ever sees it. Mostly nobody can tell.
- **Floats are a lie.** Real floats let text wrap *around* the floated box. Ours take the float and the next few paragraphs (until a heading) and put them in a two-column flex row. Wikipedia's infobox doesn't seem to mind.
- **Tables are flex rows that agree on column widths.** We estimate every column's minimum and maximum content width *before layout*, from character counts. Then we hand every cell in a column the same flex basis and a grow factor proportional to its content, so all the rows independently land on the same column widths. It's table layout with no table layout.
- **Inline layout is hand-rolled.** GPUI's text element does one font size per block of text. The web puts superscripts in the middle of sentences. So there's [our own GPUI element](crates/render/src/inline.rs) that measures every word with GPUI's text shaper, breaks lines itself, builds CSS line boxes from ascent, descent and line-height, and paints each run at its own baseline.
- **Inline margins are space characters.** `<b style="margin-right: 5px">` gets an en space and a thin space appended. They're breakable, they survive whitespace collapsing, and they're about the right width.
- **Long pages are virtualized by tearing them apart.** Any container that could be sliced horizontally without anyone noticing (no top/bottom borders, no fixed height…) gets dissolved into top-level rows of a GPUI `list()`, and each row re-creates the containers around it as "wrappers". Wikipedia's article becomes roughly 480 independently rendered rows.
- **`position: absolute` is "absolute to your parent, probably".** CSS says the nearest positioned ancestor; Taffy only knows the parent. Usually the same box. Absolute boxes anchored to the page itself stay hidden, which mostly hides "skip to content" links and modals, which is honestly an improvement.
- **No JavaScript.** Wikipedia gets its no-JS layout. We report `(scripting: none)` to media queries and render `<noscript>` content, like a browser from 2004 that happens to support `oklch()`.
- **Mask icons are SVGs served through the app's asset system.** CSS `mask-image` icons are fetched, registered as fake app assets, and drawn with GPUI's `svg()` element tinted with the box's color, which is what a mask icon is anyway.
- **The screenshot tool doesn't take screenshots.** It asks GPUI to render the window's frame into a texture offscreen. That's how the pictures above were made, with no screen-recording permission.

## What actually works

- HTML parsing (html5ever) into our own arena tree, quirks-mode detection
- CSS: `<style>`, `<link>`, `@import`, `@media` (re-evaluated when you resize the window), `@supports`, `@layer`
- Selectors via Servo's `selectors` crate (`:is`, `:where`, `:not`, `:has`, `nth-child`…), with an ancestor bloom filter
- Cascade: origin → specificity → order, `!important`, `style=""`, presentational attributes (`bgcolor`, `<font>`, `align`…), custom properties and `var()`
- Values: `calc()`, `min()`, `max()`, `clamp()`, every common unit, colors from hex to `oklch()` and `light-dark()`
- Layout: flexbox, a grid approximation, widths/max-widths, `margin: auto`, borders, rounded corners, backgrounds, overflow scrolling
- `::before`/`::after` content, real system fonts resolved from `font-family` lists
- Images (`srcset`, background loading), CSS background images, mask icons
- Links, back/forward, `#fragment` scrolling, hover URLs
- `:hover`, `:active`, `:target` (restyling the page live), `cursor`
- `position: relative/absolute/fixed`

## What doesn't (yet)

JavaScript. Forms. `position: sticky`. Real `inline-block`. Text wrapping under floats. Text selection. Transitions. Web fonts. Dark mode. Tabs (the plan is a Chrome-style tab strip in the window frame). See [PLAN.md](PLAN.md) for the roadmap, including the part where we bolt QuickJS onto a DOM that was never meant to exist.

## Running it

```sh
cargo run --release -- https://en.wikipedia.org/wiki/Rust_(programming_language)
```

Developed on macOS. GPUI also runs on Linux and Windows, but nobody has tried this there.

| Keys | Action |
|---|---|
| Cmd-L | Address bar |
| Cmd-[ / Cmd-] | Back / forward |
| Cmd-R | Reload |

### Dev tools

```sh
# Layout rows and timings for a page
cargo run --example dump -- https://news.ycombinator.com 20

# Offscreen screenshots: render, save PNGs, quit
cargo run --features screenshot -- https://example.com --screenshot out.png [--pages 3] [--row 40] [--size 1100x800] [--resize 600x800]
```

## How it's put together

```
net ──► html ──► page (the "DOM") ──► style (cascade) ──► render (boxes → GPUI) ──► shell (window)
```

| Crate | What it does |
|---|---|
| [`net`](crates/net) | Fetching (ureq), `data:` URLs |
| [`html`](crates/html) | html5ever `TreeSink` into the page tree |
| [`page`](crates/page) | The arena tree everything else reads |
| [`style`](crates/style) | Stylesheet parsing, selector matching, the cascade, computed styles, the [UA stylesheet](crates/style/src/ua.css) |
| [`render`](crates/render) | Box tree, float and table approximations, the inline layout engine, GPUI rendering |
| [`shell`](crates/shell) | The window, toolbar, history, image loading, live restyling |

Built on [GPUI](https://www.gpui.rs) via [gpui-kit](https://gpui-kit.com), [html5ever](https://github.com/servo/html5ever), [cssparser and selectors](https://github.com/servo/stylo), and [Taffy](https://github.com/DioxusLabs/taffy) underneath GPUI.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
