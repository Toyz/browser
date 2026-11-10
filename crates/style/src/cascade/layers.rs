//! Cascade layers (`@layer`): which layer's rules win.
//!
//! Layers are ordered by where a document first mentions them, across all
//! its sheets. A later layer beats an earlier one; rules in no layer beat
//! every layer; and a layer's own rules beat the layers nested in it
//! (`a` over `a.b`). `!important` reverses the order (see `Styler::cascade`).

use super::*;

/// The rank of rules in no layer (and of `style=""` and attribute hints).
pub(super) const UNLAYERED: u16 = u16::MAX;

/// Layer names in the order they were declared.
#[derive(Default)]
pub(super) struct LayerOrder {
    names: Vec<Arc<str>>,
}

impl LayerOrder {
    pub(super) fn declare(&mut self, name: &Arc<str>) {
        if !self.names.contains(name) {
            self.names.push(name.clone());
        }
    }

    /// Where `name`'s own rules sort: the declaration index of each
    /// segment's layer among its siblings, then a slot after all of that
    /// layer's children.
    fn key(&self, name: &str) -> Vec<u32> {
        let mut key = Vec::new();
        let mut prefix = String::new();
        for segment in name.split('.') {
            let parent = prefix.clone();
            if !prefix.is_empty() {
                prefix.push('.');
            }
            prefix.push_str(segment);
            // Index among the layers declared directly inside `parent`.
            let siblings = self.names.iter().filter(|n| match n.rsplit_once('.') {
                Some((p, _)) => p == parent,
                None => parent.is_empty(),
            });
            let index = siblings.clone().position(|n| **n == *prefix).unwrap_or(siblings.count());
            key.push(index as u32);
        }
        key.push(u32::MAX);
        key
    }

    /// Give every rule its layer's rank (unlayered rules get [`UNLAYERED`]).
    pub(super) fn rank(&self, rules: &mut [StyleRule]) {
        if self.names.is_empty() {
            for rule in rules {
                rule.layer_rank = UNLAYERED;
            }
            return;
        }
        let mut keys: Vec<(Vec<u32>, &Arc<str>)> = self.names.iter().map(|n| (self.key(n), n)).collect();
        keys.sort();
        let rank: HashMap<&str, u16> = keys.iter().enumerate().map(|(i, (_, n))| (&***n, i.min(UNLAYERED as usize - 1) as u16)).collect();
        for rule in rules {
            rule.layer_rank = rule.layer.as_deref().and_then(|l| rank.get(l).copied()).unwrap_or(UNLAYERED);
        }
    }
}
