//! The plugins the app knows about, by effect id.

use crate::cache::ScanCache;
use crate::traits::PluginDescriptor;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default)]
pub struct Catalog {
    /// Every plugin found, enabled or not (for the Plugin Manager's list).
    known: BTreeMap<String, PluginDescriptor>,
    /// The enabled ones: what menus and racks offer.
    by_id: BTreeMap<String, PluginDescriptor>,
    /// Plugins added directly (bundled with a format, or registered by tests), which a refresh
    /// from the scan cache must not drop.
    pinned: BTreeMap<String, PluginDescriptor>,
    /// Bumped whenever the contents change, so menus know when to rebuild.
    pub generation: u64,
}

impl Catalog {
    /// Rebuild from the cache and the pinned plugins, leaving out disabled ones.
    pub fn refresh(&mut self, cache: &ScanCache, disabled: &BTreeSet<String>) {
        let known: BTreeMap<_, _> = cache
            .descriptors()
            .map(|d| (d.effect_id(), d.clone()))
            .chain(self.pinned.clone())
            .collect();
        let by_id: BTreeMap<_, _> = known
            .iter()
            .filter(|(id, _)| !disabled.contains(*id))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if by_id != self.by_id || known != self.known {
            self.by_id = by_id;
            self.known = known;
            self.generation += 1;
        }
    }

    /// Add a plugin that no scan provides; it stays through refreshes.
    pub fn insert(&mut self, d: PluginDescriptor) {
        self.pinned.insert(d.effect_id(), d.clone());
        self.known.insert(d.effect_id(), d.clone());
        self.by_id.insert(d.effect_id(), d);
        self.generation += 1;
    }

    pub fn get(&self, effect_id: &str) -> Option<&PluginDescriptor> {
        self.by_id.get(effect_id)
    }

    /// Every plugin found, including disabled ones.
    pub fn known(&self) -> impl Iterator<Item = &PluginDescriptor> {
        self.known.values()
    }

    pub fn all(&self) -> impl Iterator<Item = &PluginDescriptor> {
        self.by_id.values()
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Vendor → plugins, both sorted by name, for the Plugins ▸ Vendor ▸ Name menu.
    pub fn by_vendor(&self) -> Vec<(String, Vec<&PluginDescriptor>)> {
        let mut map: BTreeMap<String, Vec<&PluginDescriptor>> = BTreeMap::new();
        for d in self.by_id.values() {
            let vendor = if d.vendor.is_empty() {
                "Other"
            } else {
                &d.vendor
            };
            map.entry(vendor.to_string()).or_default().push(d);
        }
        for list in map.values_mut() {
            list.sort_by_key(|d| d.name.to_lowercase());
        }
        map.into_iter().collect()
    }
}
