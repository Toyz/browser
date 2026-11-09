//! Page model → native GPUI elements.
//!
//! [`boxes`] builds a plain-data layout tree (off the main thread); [`view`]
//! turns its rows into GPUI elements on demand, one row per virtualized list item.

pub mod boxes;
pub mod images;
pub mod inline;
pub mod view;

pub use boxes::{LayoutTree, build, build_plain_text};
pub use images::{ImageState, ImageStore, PageAssets};
pub use view::{BlockHover, FontBook, InlineHover, LinkClick, LinkHover, Palette, RenderCx, render_row};
