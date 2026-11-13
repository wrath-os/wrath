//! The uids and gids a configuration's lock keeps, so a user or group keeps its number as others
//! come and go, and a removed one's number isn't handed to someone new.

use std::collections::BTreeMap;

use crate::Value;

/// The lock file's name in a configuration directory.
pub const LOCK_FILE: &str = "wrath.lock";

/// Numbers by user and group name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ids {
    pub users: BTreeMap<String, i64>,
    pub groups: BTreeMap<String, i64>,
}

impl Ids {
    pub fn is_empty(&self) -> bool {
        self.users.is_empty() && self.groups.is_empty()
    }

    /// The ids in a lock file's text, from its `[ids]` table.
    pub fn from_lock(text: &str) -> Result<Self, String> {
        let lock: toml::Table = text
            .parse()
            .map_err(|e: toml::de::Error| e.message().to_owned())?;
        let Some(ids) = lock.get("ids") else {
            return Ok(Self::default());
        };
        let ids = ids.as_table().ok_or("`ids` isn't a table")?;
        if let Some(other) = ids.keys().find(|k| *k != "users" && *k != "groups") {
            return Err(format!("unknown field `ids.{other}`"));
        }
        let table = |name: &str| -> Result<BTreeMap<String, i64>, String> {
            let Some(ids) = ids.get(name) else {
                return Ok(BTreeMap::new());
            };
            let ids = ids
                .as_table()
                .ok_or(format!("`ids.{name}` isn't a table"))?;
            ids.iter()
                .map(|(id_of, id)| match id.as_integer() {
                    Some(id) => Ok((id_of.clone(), id)),
                    None => Err(format!("`ids.{name}.{id_of}` isn't a number")),
                })
                .collect()
        };
        Ok(Self {
            users: table("users")?,
            groups: table("groups")?,
        })
    }

    /// As modules see them: `{"users": {...}, "groups": {...}}`.
    pub fn to_value(&self) -> Value {
        let table = |ids: &BTreeMap<String, i64>| {
            Value::Table(
                ids.iter()
                    .map(|(name, id)| (name.clone(), Value::Int(*id)))
                    .collect(),
            )
        };
        Value::Table(BTreeMap::from([
            ("users".to_owned(), table(&self.users)),
            ("groups".to_owned(), table(&self.groups)),
        ]))
    }

    /// The ids in a value shaped as [`Ids::to_value`] makes it, or `None` for another shape.
    pub fn from_value(value: &Value) -> Option<Self> {
        let Value::Table(fields) = value else {
            return None;
        };
        let table = |name: &str| -> Option<BTreeMap<String, i64>> {
            let Some(Value::Table(ids)) = fields.get(name) else {
                return None;
            };
            ids.iter()
                .map(|(name, id)| match id {
                    Value::Int(id) => Some((name.clone(), *id)),
                    _ => None,
                })
                .collect()
        };
        Some(Self {
            users: table("users")?,
            groups: table("groups")?,
        })
    }

    /// `self` with what `newer` adds or changes. An id another name already has is an error,
    /// such as two roles each giving a new user the same uid.
    pub fn merged(&self, newer: &Self) -> Result<Self, String> {
        let mut merged = self.clone();
        for (kind, mine, theirs) in [
            ("uid", &mut merged.users, &newer.users),
            ("gid", &mut merged.groups, &newer.groups),
        ] {
            for (name, id) in theirs {
                if let Some((other, _)) = mine.iter().find(|(o, i)| *i == id && *o != name) {
                    return Err(format!(
                        "`{name}` and `{other}` both have {kind} {id}; set one's explicitly"
                    ));
                }
                mine.insert(name.clone(), *id);
            }
        }
        Ok(merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(users: &[(&str, i64)], groups: &[(&str, i64)]) -> Ids {
        let map =
            |pairs: &[(&str, i64)]| pairs.iter().map(|(n, i)| ((*n).to_owned(), *i)).collect();
        Ids {
            users: map(users),
            groups: map(groups),
        }
    }

    #[test]
    fn a_lock_without_ids_has_none_and_one_with_them_is_read() {
        assert!(Ids::from_lock("version = 3\n").unwrap().is_empty());
        let text = "version = 4\n[ids.users]\nalice = 1000\n[ids.groups]\nvideo = 400\n";
        assert_eq!(
            Ids::from_lock(text).unwrap(),
            ids(&[("alice", 1000)], &[("video", 400)])
        );
        assert!(Ids::from_lock("[ids.users]\nalice = \"x\"\n").is_err());
    }

    #[test]
    fn ids_go_to_a_value_and_back() {
        let both = ids(&[("alice", 1000), ("bob", 1001)], &[("video", 400)]);
        assert_eq!(Ids::from_value(&both.to_value()), Some(both));
        assert_eq!(Ids::from_value(&Value::Int(1)), None);
    }

    #[test]
    fn merging_keeps_whats_gone_takes_changes_and_refuses_a_shared_number() {
        let old = ids(&[("alice", 1000), ("gone", 1001)], &[]);
        let new = ids(&[("alice", 1005), ("carol", 1002)], &[("video", 400)]);
        assert_eq!(
            old.merged(&new).unwrap(),
            ids(
                &[("alice", 1005), ("carol", 1002), ("gone", 1001)],
                &[("video", 400)]
            )
        );
        let clash = ids(&[("dave", 1001)], &[]);
        assert_eq!(
            old.merged(&clash).unwrap_err(),
            "`dave` and `gone` both have uid 1001; set one's explicitly"
        );
    }
}
