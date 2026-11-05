//! Ordered lists: named items placed with `before` and `after` anchors instead of numbers.
//!
//! Items are tables with a `name`. An item can say it goes `before` or `after` other items, by
//! name. The merged list is a topological sort of those constraints; items that aren't
//! constrained relative to each other are ordered by name, so the result never depends on the
//! order files or modules were read in.

use std::collections::{BTreeMap, BTreeSet};

use crate::value::Value;

pub(crate) const NAME: &str = "name";
pub(crate) const BEFORE: &str = "before";
pub(crate) const AFTER: &str = "after";

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Item {
    pub name: String,
    pub before: Vec<String>,
    pub after: Vec<String>,
    /// The item without its anchors.
    pub value: Value,
}

/// Splits an item into its name, anchors and the rest.
pub(crate) fn split(item: &Value) -> Result<Item, String> {
    let Value::Table(entries) = item else {
        return Err(format!("expected a table with a `name`, found {item}"));
    };
    let Some(Value::Str(name)) = entries.get(NAME) else {
        return Err(format!(
            "expected a table with a `name` string, found {item}"
        ));
    };
    let anchors = |key: &str| match entries.get(key) {
        None => Ok(Vec::new()),
        Some(Value::Str(anchor)) => Ok(vec![anchor.clone()]),
        Some(Value::List(anchors)) => anchors
            .iter()
            .map(|anchor| match anchor {
                Value::Str(anchor) => Ok(anchor.clone()),
                other => Err(format!("`{key}` names items, found {other}")),
            })
            .collect(),
        Some(other) => Err(format!("`{key}` names items, found {other}")),
    };
    let mut value = entries.clone();
    value.remove(BEFORE);
    value.remove(AFTER);
    Ok(Item {
        name: name.clone(),
        before: anchors(BEFORE)?,
        after: anchors(AFTER)?,
        value: Value::Table(value),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OrderError {
    /// An anchor names an item that isn't in the list.
    UnknownAnchor { item: String, anchor: String },
    /// The anchors contradict each other; these items are on a cycle.
    Cycle(Vec<String>),
}

/// Orders items by their anchors, breaking ties by name, and leaves out the `removed` names.
/// Removed items still take part in the ordering, so items anchored around them keep their order
/// relative to each other. Names must be unique.
pub(crate) fn order(items: &[Item], removed: &BTreeSet<String>) -> Result<Vec<Value>, OrderError> {
    let by_name: BTreeMap<&str, &Item> = items
        .iter()
        .map(|item| (item.name.as_str(), item))
        .collect();
    // Edges point from an item to the items that must come after it.
    let mut successors: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut indegree: BTreeMap<&str, usize> = by_name.keys().map(|name| (*name, 0)).collect();
    for item in items {
        let edges = item
            .before
            .iter()
            .map(|anchor| (item.name.as_str(), anchor.as_str()))
            .chain(
                item.after
                    .iter()
                    .map(|anchor| (anchor.as_str(), item.name.as_str())),
            );
        for (first, then) in edges {
            for name in [first, then] {
                if !by_name.contains_key(name) {
                    return Err(OrderError::UnknownAnchor {
                        item: item.name.clone(),
                        anchor: name.to_owned(),
                    });
                }
            }
            if successors.entry(first).or_default().insert(then) {
                *indegree.get_mut(then).expect("every name has an indegree") += 1;
            }
        }
    }

    let mut ready: BTreeSet<&str> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(name, _)| *name)
        .collect();
    let mut ordered = Vec::with_capacity(items.len());
    while let Some(name) = ready.pop_first() {
        if !removed.contains(name) {
            ordered.push(by_name[name].value.clone());
        }
        for then in successors.get(name).into_iter().flatten() {
            let degree = indegree.get_mut(then).expect("every name has an indegree");
            *degree -= 1;
            if *degree == 0 {
                ready.insert(then);
            }
        }
    }
    let stuck: Vec<String> = indegree
        .into_iter()
        .filter(|(_, degree)| *degree > 0)
        .map(|(name, _)| name.to_owned())
        .collect();
    if !stuck.is_empty() {
        return Err(OrderError::Cycle(stuck));
    }
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, before: &[&str], after: &[&str]) -> Item {
        let mut entries = BTreeMap::from([(NAME.to_owned(), Value::Str(name.into()))]);
        let list =
            |names: &[&str]| Value::List(names.iter().map(|n| Value::Str((*n).into())).collect());
        if !before.is_empty() {
            entries.insert(BEFORE.into(), list(before));
        }
        if !after.is_empty() {
            entries.insert(AFTER.into(), list(after));
        }
        split(&Value::Table(entries)).unwrap()
    }

    fn names(values: &[Value]) -> Vec<String> {
        values
            .iter()
            .map(|value| match value {
                Value::Table(entries) => entries[NAME].to_string().trim_matches('"').to_owned(),
                other => panic!("not an item: {other}"),
            })
            .collect()
    }

    #[test]
    fn anchors_order_items_and_names_break_ties() {
        let items = [
            item("unix", &[], &["env"]),
            item("systemd", &[], &["unix"]),
            item("env", &[], &[]),
            item("faillock", &["unix"], &["env"]),
            item("a-unrelated", &[], &[]),
        ];

        assert_eq!(
            names(&order(&items, &BTreeSet::new()).unwrap()),
            ["a-unrelated", "env", "faillock", "unix", "systemd"]
        );
    }

    #[test]
    fn the_result_does_not_depend_on_input_order() {
        let mut items = vec![
            item("c", &[], &[]),
            item("b", &["c"], &[]),
            item("a", &[], &["c"]),
        ];
        let first = order(&items, &BTreeSet::new()).unwrap();
        items.reverse();

        assert_eq!(order(&items, &BTreeSet::new()).unwrap(), first);
        assert_eq!(names(&first), ["b", "c", "a"]);
    }

    #[test]
    fn anchors_are_removed_from_the_items() {
        let ordered = order(
            &[item("a", &[], &["b"]), item("b", &[], &[])],
            &BTreeSet::new(),
        )
        .unwrap();

        assert_eq!(
            ordered[1],
            Value::Table(BTreeMap::from([(NAME.to_owned(), Value::Str("a".into()))]))
        );
    }

    #[test]
    fn removed_items_keep_their_neighbours_in_order() {
        let items = [
            item("env", &[], &[]),
            item("unix", &[], &["env"]),
            item("deny", &[], &["unix"]),
        ];
        let removed = BTreeSet::from(["unix".to_owned()]);

        assert_eq!(names(&order(&items, &removed).unwrap()), ["env", "deny"]);
    }

    #[test]
    fn reports_unknown_anchors_and_cycles() {
        assert_eq!(
            order(&[item("a", &["missing"], &[])], &BTreeSet::new()),
            Err(OrderError::UnknownAnchor {
                item: "a".into(),
                anchor: "missing".into()
            })
        );
        assert_eq!(
            order(
                &[
                    item("a", &["b"], &[]),
                    item("b", &["a"], &[]),
                    item("c", &[], &[])
                ],
                &BTreeSet::new()
            ),
            Err(OrderError::Cycle(vec!["a".into(), "b".into()]))
        );
    }

    #[test]
    fn rejects_malformed_items() {
        assert_eq!(
            split(&Value::Str("x".into())),
            Err("expected a table with a `name`, found \"x\"".into())
        );
        let bad_anchor = Value::Table(BTreeMap::from([
            (NAME.to_owned(), Value::Str("a".into())),
            (BEFORE.to_owned(), Value::Int(1)),
        ]));
        assert_eq!(
            split(&bad_anchor),
            Err("`before` names items, found 1".into())
        );
    }
}
