//! Box size estimates made while styling (container query units and percentages need one before layout).

use super::*;

impl Styler {
    /// Guess the element's content size from its style and its parent's
    /// guess, and make it the container-unit basis below a query container.
    pub(super) fn estimate_size(&self, s: &mut ComputedStyle, parent: &ComputedStyle, root: bool) {
        let parent_w = if root { Some(self.env.width) } else { parent.est_width };
        let parent_h = if root { Some(self.env.height) } else { parent.est_height };
        let edges_w = s.padding.left.px_or_zero() + s.padding.right.px_or_zero() + s.border_width.left + s.border_width.right;
        let edges_h = s.padding.top.px_or_zero() + s.padding.bottom.px_or_zero() + s.border_width.top + s.border_width.bottom;
        let content = |outer: f32, edges: f32| if s.border_box { (outer - edges).max(0.0) } else { outer };
        let floating = s.float != Float::None || matches!(s.position, Position::Absolute | Position::Fixed);
        // A grid item's area is a share of the grid's columns.
        // (`repeat(auto-fill, minmax(min, 1fr))`: as many as fit.)
        let auto_cols = match (parent.grid_auto, parent_w) {
            (Some((min, _)), Some(pw)) if min > 0.0 => {
                let gap = parent.column_gap.px_or_zero();
                Some((((pw + gap) / (min + gap)).floor().max(1.0)) as u16)
            }
            _ => None,
        };
        let parent_w = match (parent.display, parent.grid_columns.or(auto_cols), parent_w) {
            (Display::Grid | Display::InlineGrid, Some(cols), Some(pw)) if cols > 1 && !floating => {
                let gap = parent.column_gap.px_or_zero();
                let span = match s.grid_column {
                    Some(crate::GridSpan::Full) => cols,
                    Some(crate::GridSpan::Span(n)) => n.clamp(1, cols),
                    None => 1,
                } as f32;
                let track = (pw - gap * (cols as f32 - 1.0)) / cols as f32;
                Some(track * span + gap * (span - 1.0))
            }
            _ => parent_w,
        };
        s.est_width = match s.width {
            Length::Px(w) => Some(content(w, edges_w)),
            Length::Percent(p) => parent_w.map(|pw| content(pw * p / 100.0, edges_w)),
            Length::Auto if s.display.is_block_level() && !floating => parent_w.map(|pw| {
                (pw - s.margin.left.px_or_zero() - s.margin.right.px_or_zero() - edges_w).max(0.0)
            }),
            // Inline boxes don't size the blocks inside them: those span
            // the containing block (`<li><span><div>` chains).
            Length::Auto if matches!(s.display, Display::Inline | Display::Contents) => parent_w,
            Length::Auto => None,
        };
        if let (Some(w), Length::Px(max)) = (s.est_width, s.max_width) {
            s.est_width = Some(w.min(content(max, edges_w)));
        }
        s.est_height = match s.height {
            Length::Px(h) => Some(content(h, edges_h)),
            Length::Percent(p) => parent_h.map(|ph| content(ph * p / 100.0, edges_h)),
            Length::Auto if matches!(s.display, Display::Inline | Display::Contents) => parent_h,
            Length::Auto => None,
        };
        if s.container_type > 0 {
            let fallback = s.query_container.unwrap_or((self.env.width, self.env.height));
            let w = s.est_width.unwrap_or(fallback.0);
            let h = if s.container_type == 2 { s.est_height.unwrap_or(fallback.1) } else { fallback.1 };
            s.query_container = Some((w, h));
        }
    }
}
