use arrayvec::ArrayVec;
use itertools::Itertools;
use std::{cell::Cell, fmt};

use boa_gc::GcRefCell;
use boa_macros::{Finalize, Trace};

use crate::{
    JsString,
    object::shape::{Shape, WeakShape, slot::{Slot, SlotAttributes}},
};

#[cfg(test)]
mod tests;

pub(crate) const PIC_CAPACITY: usize = 4;

/// A cached shape-to-slot mapping for a polymorphic inline cache.
#[derive(Clone, Debug, Trace, Finalize)]
pub(crate) struct CacheEntry {
    /// A weak reference is kept to the shape to avoid the shape preventing deallocation.
    pub(crate) shape: WeakShape,
    #[unsafe_ignore_trace]
    pub(crate) slot: Slot,
    /// Feather: for a property found on the prototype, the prototype's
    /// shape when it was cached. A prototype that loses a property gets a
    /// new shape with its slots renumbered, while the receiver's shape (the
    /// cache key) stays: without this check the cache read whatever moved
    /// into the old slot (DuckDuckGo's `arr.splice(i, 1)` called
    /// `toLocaleString`, forever, once polyfills had reshaped
    /// `Array.prototype`).
    pub(crate) prototype: Option<WeakShape>,
}

/// The shape of `shape`'s prototype, if it has one.
fn prototype_shape(shape: &Shape) -> Option<Shape> {
    shape.prototype().map(|p| p.borrow().shape().clone())
}

/// Feather: whether an object still has the shape it had before a property
/// access ran. A getter (or setter) can redefine the very property it
/// belongs to (a lazy getter replacing itself with its value), reshaping the
/// object; the slot the access found is then the old shape's, and caching it
/// under the new one had later reads call the value as the getter: "not a
/// callable function" (Google Translate never showed a translation).
pub(crate) fn same_shape(before: &Shape, now: &Shape) -> bool {
    before.to_addr_usize() == now.to_addr_usize()
}

/// An inline cache entry for a property access.
#[derive(Clone, Debug, Trace, Finalize)]
pub(crate) struct InlineCache {
    /// The property that is accessed.
    pub(crate) name: JsString,

    /// Multiple cached shape-to-slot entries.
    ///
    /// Feather: allocated when the first entry is cached. Every property
    /// access of every compiled function has a cache, and most code never
    /// runs: inline, four entries each were 84 MB on YouTube.
    pub(crate) entries: GcRefCell<Option<Box<ArrayVec<CacheEntry, PIC_CAPACITY>>>>,

    /// Whether this access site has seen too many shapes and should no longer be cached.
    #[unsafe_ignore_trace]
    pub(crate) megamorphic: Cell<bool>,
}

impl fmt::Display for InlineCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "(name:{} entries:", self.name.display_escaped())?;

        if self.megamorphic.get() {
            return write!(f, "(megamorphic))");
        }

        let entries = self.entries.borrow();
        let entries = entries.iter().flat_map(|e| e.iter()).map(|e| e.shape.to_addr_usize()).format(", ");

        write!(f, "({entries:#x}))")
    }
}

impl InlineCache {
    pub(crate) fn new(name: JsString) -> Self {
        Self {
            name,
            entries: GcRefCell::new(None),
            megamorphic: Cell::new(false),
        }
    }

    pub(crate) fn set(&self, shape: &Shape, slot: Slot) {
        if self.megamorphic.get() {
            return;
        }

        let mut entries = self.entries.borrow_mut();
        let entries = entries.get_or_insert_with(Box::default);

        let prototype = if slot.attributes.contains(SlotAttributes::PROTOTYPE) {
            match prototype_shape(shape) {
                Some(p) => Some(WeakShape::from(&p)),
                None => return,
            }
        } else {
            None
        };

        // Add a new entry if there's space.
        if entries
            .try_push(CacheEntry {
                shape: shape.into(),
                slot,
                prototype,
            })
            .is_err()
        {
            // Polymorphic cache is full, transition to megamorphic.
            self.megamorphic.set(true);
            entries.clear();
        }
    }

    /// Returns the cached `(Shape, Slot)` if a matching shape exists in the inline cache.
    ///
    /// Opportunistically cleans up stale weak shape references during lookup.
    pub(crate) fn get(&self, shape: &Shape) -> Option<(Shape, Slot)> {
        if self.megamorphic.get() {
            return None;
        }

        let mut entries = self.entries.borrow_mut();
        let Some(entries) = entries.as_mut() else { return None };
        let mut i = 0;
        let mut result = None;
        let shape_addr = shape.to_addr_usize();

        while i < entries.len() {
            if let Some(upgraded) = entries[i].shape.upgrade() {
                if upgraded.to_addr_usize() == shape_addr {
                    // A prototype hit holds only while the prototype's
                    // layout is the one cached.
                    let current = match &entries[i].prototype {
                        None => true,
                        Some(cached) => match (cached.upgrade(), prototype_shape(&upgraded)) {
                            (Some(cached), Some(now)) => cached.to_addr_usize() == now.to_addr_usize(),
                            _ => false,
                        },
                    };
                    if current {
                        result = Some((upgraded, entries[i].slot));
                    } else {
                        entries.swap_remove(i);
                    }
                    break;
                }
                i += 1;
            } else {
                // Opportunistically clean up stale weak shapes.
                entries.swap_remove(i);
            }
        }

        result
    }
}
