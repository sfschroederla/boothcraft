//! The plugins the app knows about, by effect id.

use crate::cache::ScanCache;
use crate::traits::PluginDescriptor;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default)]
pub struct Catalog {
    by_id: BTreeMap<String, PluginDescriptor>,
    /// Bumped whenever the contents change, so menus know when to rebuild.
    pub generation: u64,
}

impl Catalog {
    /// Rebuild from the cache, leaving out disabled plugins.
    pub fn refresh(&mut self, cache: &ScanCache, disabled: &BTreeSet<String>) {
        let by_id: BTreeMap<_, _> = cache
            .descriptors()
            .map(|d| (d.effect_id(), d.clone()))
            .filter(|(id, _)| !disabled.contains(id))
            .collect();
        if by_id != self.by_id {
            self.by_id = by_id;
            self.generation += 1;
        }
    }

    pub fn insert(&mut self, d: PluginDescriptor) {
        self.by_id.insert(d.effect_id(), d);
        self.generation += 1;
    }

    pub fn get(&self, effect_id: &str) -> Option<&PluginDescriptor> {
        self.by_id.get(effect_id)
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
