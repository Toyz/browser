//! CSS transitions and `@keyframes` animations.
//!
//! The page's layout always holds the *target* styles (where hover patches
//! and restyles put them); while anything animates, each frame patches the
//! in-between paint values into a copy of it for display (`render::hover::
//! apply`), and asks GPUI for another frame. Nothing is re-laid out, which
//! is why only paint properties animate.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::*;
use page::NodeId;
use render::LayoutTree;
use style::{AnimDirection, Animation, ComputedStyle, Easing};

use super::Tab;

struct Transition {
    from: Arc<ComputedStyle>,
    to: Arc<ComputedStyle>,
    start: Instant,
    delay: f32,
    duration: f32,
    easing: Easing,
}

impl Transition {
    /// (style now, still running?)
    fn at(&self, now: Instant) -> (ComputedStyle, bool) {
        let elapsed = now.duration_since(self.start).as_secs_f32() - self.delay;
        let t = (elapsed / self.duration).clamp(0.0, 1.0);
        (render::animate::mix(&self.from, &self.to, self.easing.apply(t)), t < 1.0)
    }
}

struct KeyframeRun {
    node: NodeId,
    spec: Animation,
    /// (offset, style) per keyframe, computed when the animation starts.
    frames: Vec<(f32, ComputedStyle)>,
    start: Instant,
}

impl KeyframeRun {
    /// The style now, or `None` when it isn't applying (before its delay
    /// without backwards fill, or finished without forwards fill).
    fn at(&self, now: Instant) -> Option<ComputedStyle> {
        let spec = &self.spec;
        let elapsed = now.duration_since(self.start).as_secs_f32() - spec.delay;
        let total = spec.iterations * spec.duration;
        let (iteration, frac) = if elapsed < 0.0 {
            if !spec.fill_backwards {
                return None;
            }
            (0.0, 0.0)
        } else if elapsed >= total {
            if !spec.fill_forwards {
                return None;
            }
            // The end of the last iteration.
            let last = (spec.iterations - 1.0).max(0.0).ceil();
            let frac = if spec.iterations.fract() == 0.0 { 1.0 } else { spec.iterations.fract() };
            (last, frac)
        } else {
            let i = (elapsed / spec.duration).floor();
            (i, elapsed / spec.duration - i)
        };
        let reversed = match spec.direction {
            AnimDirection::Normal => false,
            AnimDirection::Reverse => true,
            AnimDirection::Alternate => !(iteration as u64).is_multiple_of(2),
            AnimDirection::AlternateReverse => (iteration as u64).is_multiple_of(2),
        };
        let p = spec.easing.apply(if reversed { 1.0 - frac } else { frac });
        // The keyframe segment around p.
        let i = self.frames.iter().rposition(|(o, _)| *o <= p).unwrap_or(0);
        let (o0, s0) = &self.frames[i];
        let Some((o1, s1)) = self.frames.get(i + 1) else { return Some(s0.clone()) };
        let local = if o1 > o0 { (p - o0) / (o1 - o0) } else { 1.0 };
        Some(render::animate::mix(s0, s1, local))
    }

    fn finished(&self, now: Instant) -> bool {
        let spec = &self.spec;
        spec.iterations.is_finite()
            && !spec.fill_forwards
            && now.duration_since(self.start).as_secs_f32() > spec.delay + spec.iterations * spec.duration
    }
}

#[derive(Default)]
pub struct Animations {
    transitions: HashMap<NodeId, Transition>,
    keyframes: Vec<KeyframeRun>,
    /// The layout with this frame's in-between styles.
    pub display: Option<Arc<LayoutTree>>,
}

impl Animations {
    pub fn is_running(&self) -> bool {
        !self.transitions.is_empty() || !self.keyframes.is_empty()
    }
}

impl Tab {
    /// Hover (or another state change) just changed these elements' paint:
    /// animate the ones with `transition` for what changed.
    pub(super) fn start_transitions(&mut self, changed: Vec<(NodeId, Arc<ComputedStyle>, Arc<ComputedStyle>)>) {
        let now = Instant::now();
        for (node, old, new) in changed {
            let props = render::animate::changed_properties(&old, &new);
            let timing = new
                .transitions
                .as_ref()
                .and_then(|t| props.iter().find_map(|p| t.for_property(p)).or_else(|| t.for_property("all")));
            let Some((duration, delay, easing)) = timing else {
                self.animations.transitions.remove(&node);
                continue;
            };
            // Interrupted mid-way (the pointer left again): start from where
            // it is now, like browsers do.
            let from = match self.animations.transitions.get(&node) {
                Some(running) => Arc::new(running.at(now).0),
                None => old,
            };
            self.animations
                .transitions
                .insert(node, Transition { from, to: new, start: now, delay, duration, easing });
        }
    }

    /// The page (or its styles) changed: start `@keyframes` animations for
    /// elements that have them, keeping the ones already running.
    pub(super) fn sync_keyframes(&mut self, window: &Window) {
        let Some(page) = &self.page else { return };
        let elements = render::hover::animated_elements(&page.layout);
        if elements.is_empty() && self.animations.keyframes.is_empty() {
            return;
        }
        let doc = page.doc.clone();
        let mut old = std::mem::take(&mut self.animations.keyframes);
        let Some(styler) = self.styler(window) else { return };
        let now = Instant::now();
        let mut keep: Vec<KeyframeRun> = Vec::new();
        for (node, base) in elements {
            for spec in base.animations.iter().flat_map(|a| a.iter()) {
                if spec.duration <= 0.0 {
                    continue;
                }
                if let Some(i) = old.iter().position(|r| r.node == node && r.spec.name == spec.name) {
                    keep.push(old.swap_remove(i));
                    continue;
                }
                // Custom properties (`--angle`) restyle the element per
                // frame: sample them (a few per 100ms of the cycle).
                if let (Some(custom), Some(doc)) = (styler.keyframe_custom(&spec.name), doc.as_deref()) {
                    let samples = ((spec.duration * 30.0) as usize).clamp(8, 120);
                    let frames = render::hover::custom_property_frames(doc, styler, node, &custom, samples);
                    keep.push(KeyframeRun { node, spec: spec.clone(), frames, start: now });
                    continue;
                }
                if let Some(frames) = styler.keyframe_styles(&spec.name, &base) {
                    keep.push(KeyframeRun { node, spec: spec.clone(), frames, start: now });
                }
            }
        }
        self.animations.keyframes = keep;
    }

    /// Called every render: this frame's in-between layout, and another
    /// frame requested while anything is still moving.
    pub(super) fn animation_frame(&mut self, window: &mut Window) {
        if !self.animations.is_running() {
            self.animations.display = None;
            return;
        }
        let Some(page) = &self.page else { return };
        let now = Instant::now();
        let mut styles: HashMap<NodeId, Arc<ComputedStyle>> = HashMap::new();
        self.animations.transitions.retain(|node, t| {
            let (style, running) = t.at(now);
            styles.insert(*node, Arc::new(style));
            running
        });
        // Animations override transitions.
        for run in &self.animations.keyframes {
            if let Some(style) = run.at(now) {
                styles.insert(run.node, Arc::new(style));
            }
        }
        self.animations.keyframes.retain(|r| !r.finished(now));
        self.animations.display = render::hover::apply(&page.layout, &styles).map(Arc::new);
        if self.animations.is_running() {
            window.request_animation_frame();
        }
    }
}
