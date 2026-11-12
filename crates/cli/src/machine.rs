//! This machine: the role and values it was given, at install or in `machines.toml`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Where a machine keeps the role and values it was given.
pub(crate) const MACHINE_FILE: &str = "/var/lib/distro/machine.toml";
const MACHINES: &str = "machines.toml";
const ROLE: &str = "role";
const NAME: &str = "name";
const DISKS: &str = "disks";

/// What a machine was given: `role = "worker"`, `name = "w-03"`, values such as its zone, and the
/// disks install laid out, as `[disks]` with each of the configuration's disks and its devices.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Given {
    pub role: Option<String>,
    pub name: Option<String>,
    pub values: BTreeMap<String, String>,
    pub disks: BTreeMap<String, Vec<String>>,
}

impl Given {
    /// What `file` records, or nothing when there's no such file.
    pub fn read(file: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(file) {
            Ok(text) => Self::parse(&text, &file.display().to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", file.display())),
        }
    }

    /// What `text`, the file at `at`, records.
    pub fn parse(text: &str, at: &str) -> Result<Self, String> {
        let doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{at}: {e}"))?;
        let mut given = Self::default();
        for (key, item) in doc.iter() {
            if key == DISKS {
                given.disks = disks(item)
                    .ok_or_else(|| format!("{at}: `disks` lists each disk's devices"))?;
                continue;
            }
            let value = item
                .as_str()
                .ok_or_else(|| format!("{at}: `{key}` isn't a string"))?
                .to_owned();
            match key {
                ROLE => given.role = Some(value),
                NAME => given.name = Some(value),
                _ => {
                    given.values.insert(key.to_owned(), value);
                }
            }
        }
        Ok(given)
    }

    fn to_toml(&self) -> String {
        let mut doc = toml_edit::DocumentMut::new();
        if let Some(role) = &self.role {
            doc[ROLE] = toml_edit::value(role.as_str());
        }
        if let Some(name) = &self.name {
            doc[NAME] = toml_edit::value(name.as_str());
        }
        for (key, value) in &self.values {
            doc[key.as_str()] = toml_edit::value(value.as_str());
        }
        if !self.disks.is_empty() {
            let mut disks = toml_edit::Table::new();
            for (name, devices) in &self.disks {
                disks[name.as_str()] =
                    toml_edit::value(devices.iter().collect::<toml_edit::Array>());
            }
            doc[DISKS] = toml_edit::Item::Table(disks);
        }
        doc.to_string()
    }

    /// The values as configurations refer to them: `machine.name`, `machine.zone`.
    pub fn machine_values(&self) -> impl Iterator<Item = (String, String)> + '_ {
        self.name
            .iter()
            .map(|name| (format!("machine.{NAME}"), name.clone()))
            .chain(
                self.values
                    .iter()
                    .map(|(key, value)| (format!("machine.{key}"), value.clone())),
            )
    }
}

/// The devices of each disk in `[disks]`.
fn disks(item: &toml_edit::Item) -> Option<BTreeMap<String, Vec<String>>> {
    item.as_table_like()?
        .iter()
        .map(|(name, devices)| {
            let devices = devices.as_array()?;
            let devices = devices.iter().map(|d| d.as_str().map(str::to_owned));
            Some((name.to_owned(), devices.collect::<Option<Vec<_>>>()?))
        })
        .collect()
}

/// Writes `given` to `file`, as what a machine was given at install.
pub(crate) fn record(file: &Path, given: &Given) -> Result<(), String> {
    Record {
        file: file.to_path_buf(),
        given: given.clone(),
    }
    .write()
}

/// A machine's entry in `machines.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Listed {
    pub name: String,
    pub role: String,
    pub values: BTreeMap<String, String>,
}

/// The entry for `name` in the `machines.toml` of `config`, if it has one. `check` reports what's
/// wrong with the file, so an entry that can't be read is left out.
pub(crate) fn listed(config: &Path, name: &str) -> Option<Listed> {
    let text = std::fs::read_to_string(config.join(MACHINES)).ok()?;
    let doc: toml_edit::DocumentMut = text.parse().ok()?;
    let entry = doc.get(name)?.as_table_like()?;
    let mut values = BTreeMap::new();
    let mut role = None;
    for (key, item) in entry.iter() {
        let value = item.as_str()?.to_owned();
        if key == ROLE {
            role = Some(value);
        } else {
            values.insert(key.to_owned(), value);
        }
    }
    Some(Listed {
        name: name.to_owned(),
        role: role?,
        values,
    })
}

/// This machine's name: the one it was given, or else its hostname.
pub(crate) fn name(given: &Given) -> Option<String> {
    given.name.clone().or_else(|| {
        let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname").ok()?;
        Some(hostname.trim().to_owned()).filter(|name| !name.is_empty())
    })
}

/// The role a machine runs when it isn't given one: the role it was installed as, or else the
/// role of its entry in `machines.toml`.
pub(crate) fn role(given: &Given, listed: Option<&Listed>) -> Option<String> {
    given
        .role
        .clone()
        .or_else(|| listed.map(|listed| listed.role.clone()))
}

/// What a machine is given once it switches to `role`: what it records, with its entry in
/// `machines.toml` over it. The entry's role has to agree with the role it runs.
pub(crate) fn merge(given: Given, listed: Option<Listed>, role: &str) -> Result<Given, String> {
    let Some(listed) = listed else {
        return Ok(given);
    };
    if listed.role != role {
        return Err(format!(
            "`machines.toml` lists {} as {}, but it runs {role}",
            listed.name, listed.role
        ));
    }
    let mut values = given.values;
    values.extend(listed.values);
    Ok(Given {
        role: Some(listed.role),
        name: Some(listed.name),
        values,
        disks: given.disks,
    })
}

/// What a machine records once it switches.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Record {
    file: PathBuf,
    given: Given,
}

impl Record {
    pub fn write(&self) -> Result<(), String> {
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&self.file, self.given.to_toml())
            .map_err(|e| format!("{}: {e}", self.file.display()))
    }
}

impl std::fmt::Display for Record {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let given = self.given.to_toml();
        let given: Vec<&str> = given.lines().collect();
        write!(
            f,
            "this machine is given {} from `machines.toml`",
            given.join(", ")
        )
    }
}

/// Checks this machine is given every value in `uses` before it switches to `role`, with its
/// entry in `machines.toml` over what `file` records. Returns what to record before switching,
/// when that changes. A machine without a name keeps the hostname the network gives it.
pub(crate) fn prepare(
    config: &Path,
    file: &Path,
    role: Option<&str>,
    uses: &BTreeSet<String>,
    facts: impl Iterator<Item = (String, String)>,
) -> Result<Option<Record>, String> {
    let recorded = Given::read(file)?;
    let given = match role {
        Some(role) => {
            let listed = name(&recorded).and_then(|name| listed(config, &name));
            merge(recorded.clone(), listed, role)?
        }
        None => recorded.clone(),
    };
    let available: BTreeSet<String> = facts
        .chain(given.machine_values())
        .map(|(key, _)| key)
        .collect();
    let name = format!("machine.{NAME}");
    let missing: Vec<String> = uses
        .iter()
        .filter(|used| **used != name && !available.contains(*used))
        .map(|used| format!("`{used}`"))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "this machine isn't given {}, which the configuration uses; give it at install or in \
             `machines.toml`",
            missing.join(", ")
        ));
    }
    if uses.contains(&name) && !available.contains(&name) {
        crate::style::note(
            "this machine has no name, so it keeps the hostname the network gives it",
        );
    }
    Ok((given != recorded).then(|| Record {
        file: file.to_path_buf(),
        given,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_machine_recorded_is_read_from_its_text() {
        let given = Given::parse(
            "role = \"worker\"\nname = \"w-01\"\nzone = \"eu\"\n",
            "w-01:file",
        )
        .unwrap();
        assert_eq!(given.role.as_deref(), Some("worker"));
        assert_eq!(given.name.as_deref(), Some("w-01"));
        assert!(Given::parse("", "w-01:file").unwrap().role.is_none());
        let error = Given::parse("role = 1\n", "w-01:file").unwrap_err();
        assert!(error.starts_with("w-01:file: `role`"), "{error}");
    }

    fn listed(role: &str, zone: &str) -> Listed {
        Listed {
            name: "w-03".into(),
            role: role.into(),
            values: BTreeMap::from([("zone".into(), zone.into())]),
        }
    }

    #[test]
    fn a_machine_records_its_role_name_and_values() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("machine.toml");
        assert_eq!(Given::read(&file).unwrap(), Given::default());

        std::fs::write(
            &file,
            "role = \"worker\"\nname = \"w-03\"\nzone = \"a\"\n\n[disks]\nmain = [\"/dev/vda\"]\n",
        )
        .unwrap();
        let given = Given::read(&file).unwrap();
        assert_eq!(given.role.as_deref(), Some("worker"));
        assert_eq!(given.disks["main"], ["/dev/vda"]);
        let values: Vec<(String, String)> = given.machine_values().collect();
        assert_eq!(
            values,
            [
                ("machine.name".into(), "w-03".into()),
                ("machine.zone".into(), "a".into())
            ]
        );

        std::fs::write(&file, given.to_toml()).unwrap();
        assert_eq!(Given::read(&file).unwrap(), given);
        std::fs::write(&file, "zone = 3\n").unwrap();
        assert!(
            Given::read(&file)
                .unwrap_err()
                .contains("`zone` isn't a string")
        );
    }

    #[test]
    fn machines_are_found_in_machines_toml_by_name() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(super::listed(dir.path(), "w-03"), None);
        std::fs::write(
            dir.path().join(MACHINES),
            "w-03 = { role = \"worker\", zone = \"a\" }\nbad = { zone = \"a\" }\n",
        )
        .unwrap();

        assert_eq!(
            super::listed(dir.path(), "w-03"),
            Some(listed("worker", "a"))
        );
        assert_eq!(super::listed(dir.path(), "bad"), None);
        assert_eq!(super::listed(dir.path(), "w-04"), None);
    }

    #[test]
    fn the_role_installed_comes_before_the_listed_one() {
        let installed = Given {
            role: Some("gpu-worker".into()),
            ..Given::default()
        };
        let entry = listed("worker", "a");

        assert_eq!(
            role(&installed, Some(&entry)).as_deref(),
            Some("gpu-worker")
        );
        assert_eq!(
            role(&Given::default(), Some(&entry)).as_deref(),
            Some("worker")
        );
        assert_eq!(role(&Given::default(), None), None);
    }

    #[test]
    fn the_listed_entry_decides_values_and_must_agree_on_the_role() {
        let given = Given {
            role: Some("worker".into()),
            name: Some("old".into()),
            values: BTreeMap::from([("zone".into(), "b".into()), ("rack".into(), "7".into())]),
            disks: BTreeMap::from([("main".into(), vec!["/dev/disk/by-id/nvme-a".into()])]),
        };

        assert_eq!(merge(given.clone(), None, "worker").unwrap(), given);
        let merged = merge(given.clone(), Some(listed("worker", "a")), "worker").unwrap();
        assert_eq!(merged.name.as_deref(), Some("w-03"));
        assert_eq!(merged.values["zone"], "a");
        assert_eq!(merged.values["rack"], "7");
        assert_eq!(merged.disks, given.disks);
        let err = merge(given, Some(listed("worker", "a")), "gpu-worker").unwrap_err();
        assert_eq!(
            err,
            "`machines.toml` lists w-03 as worker, but it runs gpu-worker"
        );
    }

    fn uses(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn facts() -> std::vec::IntoIter<(String, String)> {
        vec![("facts.cpu.count".to_owned(), "4".to_owned())].into_iter()
    }

    #[test]
    fn a_machine_switches_only_when_it_has_the_values_used() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state/machine.toml");
        std::fs::write(
            dir.path().join(MACHINES),
            "w-03 = { role = \"worker\", zone = \"a\" }\n",
        )
        .unwrap();
        let used = uses(&["facts.cpu.count", "machine.name", "machine.zone"]);

        let err = prepare(dir.path(), &file, None, &used, facts()).unwrap_err();
        assert!(err.contains("isn't given `machine.zone`"), "{err}");

        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "name = \"w-03\"\n").unwrap();
        let record = prepare(dir.path(), &file, Some("worker"), &used, facts())
            .unwrap()
            .unwrap();
        assert_eq!(
            record.to_string(),
            "this machine is given role = \"worker\", name = \"w-03\", zone = \"a\" from \
             `machines.toml`"
        );
        record.write().unwrap();
        assert_eq!(
            prepare(dir.path(), &file, Some("worker"), &used, facts()),
            Ok(None)
        );

        let err = prepare(dir.path(), &file, Some("gpu-worker"), &used, facts()).unwrap_err();
        assert!(err.contains("lists w-03 as worker"), "{err}");
    }

    #[test]
    fn a_machine_without_a_name_still_switches() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("machine.toml");
        let used = uses(&["machine.name"]);

        assert_eq!(prepare(dir.path(), &file, None, &used, facts()), Ok(None));
    }
}
