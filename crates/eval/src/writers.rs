//! Which modules may set something under an option, without looking through every module.

use std::collections::BTreeMap;

use crate::module::ModuleId;
use crate::path::OptionPath;

/// One path that modules write, as an index that stays valid for this table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Entry(usize);

impl Entry {
    pub(crate) fn index(self) -> usize {
        self.0
    }
}

/// The modules writing each option path. Paths sort so that everything under a path comes right
/// after it, which is what makes the lookup cheap.
pub(crate) struct Writers {
    positions: BTreeMap<OptionPath, Entry>,
    modules: Vec<Vec<ModuleId>>,
}

impl Writers {
    /// Takes what each module writes, in module order.
    pub(crate) fn new(writes: &[Vec<OptionPath>]) -> Self {
        let mut by_path: BTreeMap<OptionPath, Vec<ModuleId>> = BTreeMap::new();
        for (index, paths) in writes.iter().enumerate() {
            for path in paths {
                by_path
                    .entry(path.clone())
                    .or_default()
                    .push(ModuleId::from_index(index));
            }
        }
        let mut modules = Vec::with_capacity(by_path.len());
        let mut positions = BTreeMap::new();
        for (path, writers) in by_path {
            positions.insert(path, Entry(modules.len()));
            modules.push(writers);
        }
        Self { positions, modules }
    }

    /// How many written paths there are.
    pub(crate) fn len(&self) -> usize {
        self.modules.len()
    }

    /// The modules writing `entry`, in module order.
    pub(crate) fn modules(&self, entry: Entry) -> &[ModuleId] {
        &self.modules[entry.0]
    }

    /// The written paths that are `path`, above it or below it.
    pub(crate) fn entries(&self, path: &OptionPath) -> Vec<Entry> {
        let above = path
            .prefixes()
            .filter_map(|prefix| self.positions.get(&prefix).copied());
        let below = self
            .positions
            .range(path.clone()..)
            .take_while(|(write, _)| write.starts_with(path))
            .map(|(_, entry)| *entry);
        let mut found: Vec<Entry> = above.chain(below).collect();
        found.sort_unstable();
        found.dedup();
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Writers {
        fn of(&self, path: &OptionPath) -> Vec<ModuleId> {
            let mut found: Vec<ModuleId> = self
                .entries(path)
                .into_iter()
                .flat_map(|entry| self.modules(entry).iter().copied())
                .collect();
            found.sort_unstable();
            found.dedup();
            found
        }
    }

    fn path(text: &str) -> OptionPath {
        if text.is_empty() {
            OptionPath::root()
        } else {
            OptionPath::parse(text).unwrap()
        }
    }

    fn paths(all: &[&str]) -> Vec<OptionPath> {
        all.iter().map(|text| path(text)).collect()
    }

    fn ids(indices: &[usize]) -> Vec<ModuleId> {
        indices.iter().map(|i| ModuleId::from_index(*i)).collect()
    }

    fn writers() -> Writers {
        Writers::new(&[
            paths(&["services.nginx"]),
            paths(&["services.nginx.port", "users.alice"]),
            paths(&["services"]),
            paths(&["system.hostname"]),
            paths(&["services.nginxish"]),
            paths(&[]),
        ])
    }

    #[test]
    fn finds_writers_of_the_path_itself() {
        assert_eq!(writers().of(&path("system.hostname")), ids(&[3]));
    }

    #[test]
    fn finds_writers_above_a_path() {
        assert_eq!(writers().of(&path("services.nginx.port")), ids(&[0, 1, 2]));
    }

    #[test]
    fn finds_writers_below_a_path() {
        assert_eq!(writers().of(&path("users")), ids(&[1]));
        assert_eq!(writers().of(&path("services.nginx")), ids(&[0, 1, 2]));
    }

    #[test]
    fn a_sibling_sharing_a_prefix_of_its_name_is_not_a_writer() {
        assert_eq!(writers().of(&path("services.nginxish")), ids(&[2, 4]));
    }

    #[test]
    fn the_root_is_written_by_everything_with_writes() {
        assert_eq!(writers().of(&OptionPath::root()), ids(&[0, 1, 2, 3, 4]));
    }

    #[test]
    fn nothing_writes_an_unrelated_path() {
        assert_eq!(writers().of(&path("network.dns")), ids(&[]));
    }

    #[test]
    fn a_module_writing_twice_under_a_path_appears_once() {
        let writers = Writers::new(&[paths(&["a.b", "a.c", "a"])]);
        assert_eq!(writers.of(&path("a")), ids(&[0]));
    }

    #[test]
    fn matches_comparing_every_module() {
        let writes = vec![
            paths(&["a.b.c", "x"]),
            paths(&["a.b"]),
            paths(&["a.d", "a.b.c.d"]),
            paths(&["x.y.z"]),
            paths(&["ab"]),
        ];
        let writers = Writers::new(&writes);
        for query in [
            "", "a", "a.b", "a.b.c", "a.b.c.d", "a.d", "x", "x.y", "x.y.z", "ab", "q",
        ] {
            let query = path(query);
            let compared: Vec<ModuleId> = writes
                .iter()
                .enumerate()
                .filter(|(_, paths)| {
                    paths
                        .iter()
                        .any(|w| w.starts_with(&query) || query.starts_with(w))
                })
                .map(|(index, _)| ModuleId::from_index(index))
                .collect();
            assert_eq!(writers.of(&query), compared, "{query}");
        }
    }
}
