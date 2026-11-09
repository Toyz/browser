# Feather 🪶

**A web browser where the web page is not a web page.** Named Feather because it's so light it doesn't have a web engine.

Normal browsers parse HTML into a DOM, run it through a rendering engine, and paint the result into a rectangle. This one parses HTML, runs its CSS through a cascade we wrote ourselves, and then turns the whole page into **native [GPUI](https://www.gpui.rs) UI elements**. GPUI is the GPU UI framework behind the Zed editor. Every paragraph you see is a GPUI element. Every `<div>` is a GPUI `div()`. The DOM is a slotmap the renderer reads once and then mostly ignores.

It is cursed. It also kind of works.

![Wikipedia, rendered as native GPUI elements](docs/wikipedia.png)

![Hacker News](docs/hacker-news.png)

## Just how cursed is it

- **Blocks are flexboxes.** CSS has a whole layout mode called "block flow". We don't have it. Every block is a GPUI `flex_col()`, and we fake margin collapsing by doing the arithmetic ourselves before Taffy ever sees it. Mostly nobody can tell.
- **Floats are a lie.** Real floats let text wrap *around* the floated box. Ours take the float and the next few paragraphs (until a heading) and put them in a two-column flex row. Wikipedia's infobox doesn't seem to mind.
- **Tables are flex rows that agree on column widths.** We estimate every column's minimum and maximum content width *before layout*, from character counts. Then we hand every cell in a column the same flex basis and a grow factor proportional to its content, so all the rows independently land on the same column widths. It's table layout with no table layout.
- **Inline layout is hand-rolled.** GPUI's text element does one font size per block of text. The web puts superscripts in the middle of sentences. So there's [our own GPUI element](crates/render/src/inline.rs) that measures every word with GPUI's text shaper, breaks lines itself, builds CSS line boxes from ascent, descent and line-height, and paints each run at its own baseline.
- **Inline-blocks are laid out with last frame's homework.** A button in a sentence is an "atomic object" laid out by a nested copy of the inline engine. An inline-block with real blocks inside is a genuine GPUI subtree, but GPUI's layout engine can't be re-entered from inside text measurement, so it's laid out as an absolutely positioned child, the line breaking uses the size it had *last frame*, and if that was wrong we just ask for another frame. It settles in one. Then the subtree gets shoved into its slot in the line.
- **Inline margins are space characters.** `<b style="margin-right: 5px">` gets an en space and a thin space appended. They're breakable, they survive whitespace collapsing, and they're about the right width.
- **Long pages are virtualized by tearing them apart.** Any container that could be sliced horizontally without anyone noticing (no top/bottom borders, no fixed height…) gets dissolved into top-level rows of a GPUI `list()`, and each row re-creates the containers around it as "wrappers". Wikipedia's article becomes roughly 480 independently rendered rows.
- **`position: absolute` is "absolute to your parent, probably".** CSS says the nearest positioned ancestor; Taffy only knows the parent. Usually the same box. Absolute boxes anchored to the page itself stay hidden, which mostly hides "skip to content" links and modals, which is honestly an improvement.
- **No JavaScript.** Wikipedia gets its no-JS layout. We report `(scripting: none)` to media queries and render `<noscript>` content, like a browser from 2004 that happens to support `oklch()`.
- **Mask icons are SVGs served through the app's asset system.** CSS `mask-image` icons are fetched, registered as fake app assets, and drawn with GPUI's `svg()` element tinted with the box's color, which is what a mask icon is anyway.
- **Inline `<svg>` is serialized back into a string.** We parse the SVG into our tree, then write it back out as markup (with `currentColor` swapped for the actual color), percent-encode it into a `data:` URL, and "download" it through the image pipeline. Icons work.
- **Links load before you click them.** Rest the pointer on a link for 60ms and Feather fetches, parses, styles and lays out the whole page in the background. By the time you click, it's done. This is the closest thing it has to speculative execution.
- **Grids are sliced like bread.** A CSS grid with a hundred cards becomes fifty two-card grids stacked on top of each other, each one its own list row. They agree on column widths because they all have the same template and the same width. Nobody can tell.
- **Text shaping is shared across the whole browser.** Shaped text is cached by content, not by element, so Wikipedia's sidebar shapes once and every other Wikipedia tab gets it free.
- **Form fields are actual text editors.** Each `<input>` is a box drawn from the page's CSS with a gpui-kit text editor (the kind you'd find in a code editor's settings panel) sitting inside it. Buttons don't have click handlers: their content links to `feather-activate:<node id>`, a URL scheme that exists only to make the inline layout's link hit-testing press buttons.
- **CSS grid templates are faked twice.** GPUI only knows "N equal columns", so `grid-template-areas` layouts are placed by hand and rebuilt as flex rows. When one column holds most of the page (Wikipedia's article), it stays splittable so the page still virtualizes, and the sidebar is hung off the article's first row as an absolutely positioned box.
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

- Tabs in the title bar, Chrome-style, with favicons; an omnibox with history suggestions, inline completion and search
- Dark mode (`prefers-color-scheme`, `color-scheme`, `light-dark()`), `file://` pages and directory listings
- Forms: text fields, textareas, checkboxes, radios, selects, buttons, labels; GET and POST submission; `:checked`, `:focus`, `:disabled`…
- CSS grid templates (`grid-template-areas`, mixed track sizes), `transform: translate()`, box-generating `::before`/`::after`

## What doesn't (yet)

JavaScript. `position: sticky`. Text wrapping under floats. Text selection. Transitions. Web fonts. See [PLAN.md](PLAN.md) for the roadmap, including the part where we bolt [Boa](https://github.com/boa-dev/boa) (a JavaScript engine written in Rust, because of course) onto a DOM that was never meant to exist.

## Running it

```sh
cargo run --release -- https://en.wikipedia.org/wiki/Rust_(programming_language)
```

Developed on macOS. GPUI also runs on Linux and Windows, but nobody has tried this there.

| Keys | Action |
|---|---|
| Cmd-L | Address bar |
| Cmd-T / Cmd-W | New tab / close tab |
| Cmd-1…9, Ctrl-Tab | Switch tabs |
| Cmd-click | Open link in a background tab |
| Cmd-[ / Cmd-] | Back / forward |
| Cmd-R | Reload |

### Dev tools

```sh
# Layout rows and timings for a page
cargo run --example dump -- https://news.ycombinator.com 20

# Offscreen screenshots: render, save PNGs, quit
cargo run --features screenshot -- https://example.com [more URLs as tabs] --screenshot out.png [--pages 3] [--row 40] [--size 1100x800] [--resize 600x800] [--dark|--light] [--type "omnibox text"] [--bench FRAMES]
```

## How it's put together

```
net ──► html ──► page (the "DOM") ──► style (cascade) ──► render (boxes → GPUI) ──► shell (window)
```

| Crate | What it does |
|---|---|
| [`net`](crates/net) | Fetching (ureq), `data:` and `file:` URLs, the subresource cache |
| [`html`](crates/html) | html5ever `TreeSink` into the page tree |
| [`page`](crates/page) | The arena tree everything else reads |
| [`style`](crates/style) | Stylesheet parsing, selector matching, the cascade, computed styles, the [UA stylesheet](crates/style/src/ua.css) |
| [`render`](crates/render) | Box tree, float and table approximations, the inline layout engine, GPUI rendering |
| [`shell`](crates/shell) | Feather itself: tabs, omnibox, history, prerendering, image loading, live restyling |

Built on [GPUI](https://www.gpui.rs) via [gpui-kit](https://gpui-kit.com), [html5ever](https://github.com/servo/html5ever), [cssparser and selectors](https://github.com/servo/stylo), and [Taffy](https://github.com/DioxusLabs/taffy) underneath GPUI.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
