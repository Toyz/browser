//! Skipping what's far off screen: a block drawn last frame well outside the
//! window is drawn this frame as an empty box of the same size (its place in
//! the layout kept, its contents not built). Long pages (a video page with
//! hundreds of comments and suggestions) otherwise build every element of
//! every visible row each frame, which a playing video asks for 30-60 times
//! a second.
//!
//! What a block's bounds were is only trusted from the frame just before,
//! of the same layout tree, fonts, pictures and window size: anything else
//! renders in full (and is measured again).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use gpui_kit::{Bounds, Pixels, Size};

use crate::boxes::{Block, LayoutBox};

/// What can change where and how big boxes are drawn: the layout tree,
/// pictures and fonts loaded, the window size.
#[derive(Clone, Copy, PartialEq, Default)]
pub struct CullKey {
    pub layout: usize,
    pub images: u64,
    pub fonts: u64,
    pub viewport: (f32, f32),
}

#[derive(Default)]
pub struct Cull {
    key: Cell<CullKey>,
    /// The frame being built (bounds recorded during it carry its number).
    frame: Cell<u64>,
    /// Where each block (by id) was drawn, and in which frame.
    drawn: RefCell<HashMap<usize, (u64, Bounds<Pixels>)>>,
    /// Blocks with absolutely positioned or fixed boxes inside that are
    /// placed against something outside them (they can show anywhere).
    escapes: RefCell<HashMap<usize, bool>>,
}

impl Cull {
    /// A frame starts: forget everything if what it's drawn from changed.
    pub fn begin(&self, key: CullKey) {
        if std::env::var_os("FEATHER_DEBUG_CULL").is_some() {
            let drawn = self.drawn.borrow();
            let last = drawn.values().filter(|(f, _)| *f == self.frame.get()).count();
            let far = drawn.values().filter(|(f, b)| *f == self.frame.get() && self.far(*b)).count();
            let old = self.key.get();
            eprintln!(
                "cull: frame {} drew {last} blocks, {far} far; changed: layout {} images {} fonts {} viewport {:?}",
                self.frame.get(),
                old.layout != key.layout,
                old.images != key.images,
                old.fonts != key.fonts,
                (old.viewport != key.viewport).then_some(key.viewport)
            );
        }
        if self.key.replace(key) != key {
            self.drawn.borrow_mut().clear();
            self.escapes.borrow_mut().clear();
        }
        self.frame.set(self.frame.get() + 1);
    }

    /// The size to draw `block` at, empty, if it was far off screen last frame.
    pub(crate) fn hollow(&self, block: &Block) -> Option<Size<Pixels>> {
        let (frame, bounds) = *self.drawn.borrow().get(&block.id)?;
        if frame + 1 != self.frame.get() || !self.far(bounds) {
            return None;
        }
        (!self.escapes(block)).then_some(bounds.size)
    }

    /// Note where a block was drawn; `true` if it's near or on screen.
    pub(crate) fn drawn(&self, id: usize, bounds: Bounds<Pixels>) -> bool {
        self.drawn.borrow_mut().insert(id, (self.frame.get(), bounds));
        !self.far(bounds)
    }

    /// Entirely more than a screenful above or below the window.
    fn far(&self, bounds: Bounds<Pixels>) -> bool {
        let (_, height) = self.key.get().viewport;
        let margin = height.max(800.0);
        let (top, bottom) = (f32::from(bounds.origin.y), f32::from(bounds.origin.y + bounds.size.height));
        bottom < -margin || top > height + margin
    }

    fn escapes(&self, block: &Block) -> bool {
        if let Some(&known) = self.escapes.borrow().get(&block.id) {
            return known;
        }
        let positioned = block.style.position != style::Position::Static;
        let out = block.children.iter().any(|child| {
            let LayoutBox::Block(child) = child else { return false };
            match child.style.position {
                style::Position::Fixed => true,
                style::Position::Absolute if !positioned => true,
                _ => !positioned && self.escapes(child),
            }
        });
        self.escapes.borrow_mut().insert(block.id, out);
        out
    }
}
