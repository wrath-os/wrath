//! Comparing two models: what would change, and how disruptive applying it is.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use distro_eval::{Unit, Value, home_path};

use crate::{File, Model, Secret};

/// How much applying a change disrupts the running system, least first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Applied to the running system: files swapped, units restarted.
    Live,
    /// Takes effect when the user logs in again.
    NextLogin,
    /// Needs userspace restarted, but not the kernel.
    SoftReboot,
    Reboot,
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Live => "live",
            Self::NextLogin => "next-login",
            Self::SoftReboot => "soft-reboot",
            Self::Reboot => "reboot",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Removed,
    Changed,
}

/// One difference between two models.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    /// The option path, package name, file path or unit name.
    pub name: String,
    /// Lines describing the difference, such as `8080 -> 8081`.
    pub details: Vec<String>,
    /// `None` for options, which only matter through what they render.
    pub tier: Option<Tier>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub options: Vec<Change>,
    pub packages: Vec<Change>,
    pub files: Vec<Change>,
    pub secrets: Vec<Change>,
    pub units: Vec<Change>,
    pub base: Vec<Change>,
    pub backends: Vec<Change>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.sections().all(|(_, changes)| changes.is_empty())
    }

    /// The most disruptive tier any change needs.
    pub fn tier(&self) -> Option<Tier> {
        self.sections()
            .flat_map(|(_, changes)| changes.iter())
            .filter_map(|change| change.tier)
            .max()
    }

    fn sections(&self) -> impl Iterator<Item = (&'static str, &[Change])> {
        [
            ("options", self.options.as_slice()),
            ("packages", self.packages.as_slice()),
            ("files", self.files.as_slice()),
            ("secrets", self.secrets.as_slice()),
            ("units", self.units.as_slice()),
            ("base", self.base.as_slice()),
            ("backends", self.backends.as_slice()),
        ]
        .into_iter()
    }
}

impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return writeln!(f, "no changes");
        }
        let mut count = 0;
        for (heading, changes) in self.sections().filter(|(_, changes)| !changes.is_empty()) {
            writeln!(f, "{heading}")?;
            for change in changes {
                count += 1;
                let sign = match change.kind {
                    ChangeKind::Added => '+',
                    ChangeKind::Removed => '-',
                    ChangeKind::Changed => '~',
                };
                write!(f, "  {sign} {}", change.name)?;
                if let Some(tier) = change.tier {
                    write!(f, " (tier: {tier})")?;
                }
                match change.details.as_slice() {
                    [detail] if change.tier.is_none() => writeln!(f, ": {detail}")?,
                    details => {
                        writeln!(f)?;
                        for detail in details {
                            writeln!(f, "      {detail}")?;
                        }
                    }
                }
            }
        }
        let noun = if count == 1 { "change" } else { "changes" };
        match self.tier() {
            Some(tier) => writeln!(f, "{count} {noun}, tier: {tier}"),
            None => writeln!(f, "{count} {noun}, nothing to apply"),
        }
    }
}

/// What changes going from `before` to `after`.
pub fn plan(before: &Model, after: &Model) -> Plan {
    let file_details = |before: Option<&File>, after: Option<&File>| {
        let detail = match (before, after) {
            (Some(before), Some(after)) if before.mode != after.mode => {
                vec![format!("mode {:o} -> {:o}", before.mode, after.mode)]
            }
            _ => Vec::new(),
        };
        (detail, Some(Tier::Live))
    };
    let mut files = diff(&before.files, &after.files, file_details);
    files.extend(diff(&home_files(before), &home_files(after), file_details));
    let mut units = diff(&before.units, &after.units, |before, after| {
        (unit_details(before, after), Some(Tier::Live))
    });
    let secrets = diff(&before.secrets, &after.secrets, |before, after| {
        (secret_details(before, after), Some(Tier::Live))
    });
    units.extend(restarts(before, after, &files, &secrets));
    units.extend(triggered(after, &files));
    units.sort_by(|a, b| a.name.cmp(&b.name));
    Plan {
        options: diff(&before.options, &after.options, |before, after| {
            (vec![value_change(before, after)], None)
        }),
        packages: diff(&packages(before), &packages(after), |_, _| {
            (Vec::new(), Some(Tier::Live))
        }),
        files,
        secrets,
        units,
        base: diff_with_names(&before.base, &after.base, |name, before, after| {
            // The bootloader and kernel only change on the next boot.
            let tier = if name == "boot" {
                Tier::Reboot
            } else {
                Tier::Live
            };
            (vec![value_change(before, after)], Some(tier))
        }),
        // Nothing is known about what a backend-specific setting changes underneath.
        backends: diff(&before.backends, &after.backends, |before, after| {
            (vec![value_change(before, after)], Some(Tier::Reboot))
        }),
    }
}

/// Units that are the same in both models but restart because a file they read or a secret
/// they mention changed.
fn restarts(before: &Model, after: &Model, files: &[Change], secrets: &[Change]) -> Vec<Change> {
    after
        .units
        .iter()
        .filter(|(name, unit)| before.units.get(*name) == Some(unit))
        .filter_map(|(name, unit)| {
            let mentioned = after.secrets_of(unit);
            let changed: Vec<String> = files
                .iter()
                .map(|change| change.name.as_str())
                .filter(|path| unit.restart_on.contains(*path))
                .map(str::to_owned)
                .chain(
                    secrets
                        .iter()
                        .filter(|change| mentioned.contains(change.name.as_str()))
                        .map(|change| format!("secret {}", change.name)),
                )
                .collect();
            (!changed.is_empty()).then(|| Change {
                kind: ChangeKind::Changed,
                name: name.clone(),
                details: vec![format!("restarts: {} changed", changed.join(", "))],
                tier: Some(Tier::Live),
            })
        })
        .collect()
}

/// What changed about a secret. Its contents are never shown, only that they changed.
fn secret_details(before: Option<&Secret>, after: Option<&Secret>) -> Vec<String> {
    let (Some(before), Some(after)) = (before, after) else {
        return Vec::new();
    };
    let mut details = Vec::new();
    if before.hash != after.hash {
        details.push("contents changed".to_owned());
    }
    let owner = |secret: &Secret| match &secret.group {
        Some(group) => format!("{}:{group}", secret.owner),
        None => secret.owner.clone(),
    };
    if owner(before) != owner(after) {
        details.push(format!("owner {} -> {}", owner(before), owner(after)));
    }
    if before.mode != after.mode {
        details.push(format!("mode {:o} -> {:o}", before.mode, after.mode));
    }
    if before.link != after.link {
        let link = |secret: &Secret| secret.link.clone().unwrap_or_else(|| "none".to_owned());
        details.push(format!("link {} -> {}", link(before), link(after)));
    }
    details
}

/// Services the system provides that reload or restart because a file they read changed.
fn triggered(after: &Model, files: &[Change]) -> Vec<Change> {
    let mut services: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for ((unit, action), paths) in &after.triggers {
        let changed: Vec<&str> = files
            .iter()
            .map(|change| change.name.as_str())
            .filter(|path| paths.contains(*path))
            .collect();
        if !changed.is_empty() {
            services.entry(unit).or_default().push(format!(
                "{}: {} changed",
                action.verb(),
                changed.join(", ")
            ));
        }
    }
    services
        .into_iter()
        .map(|(unit, details)| Change {
            kind: ChangeKind::Changed,
            name: unit.to_owned(),
            details,
            tier: Some(Tier::Live),
        })
        .collect()
}

/// `old -> new`, or the one value when the other side has none.
fn value_change(before: Option<&Value>, after: Option<&Value>) -> String {
    match (before, after) {
        (Some(before), Some(after)) => format!("{before} -> {after}"),
        (None, Some(value)) | (Some(value), None) => value.to_string(),
        (None, None) => unreachable!("one side has a value"),
    }
}

/// The system's packages, and each user's as `name (for user)`.
fn packages(model: &Model) -> BTreeMap<String, ()> {
    let users = model.homes.iter().flat_map(|(user, home)| {
        home.packages
            .iter()
            .map(move |package| format!("{package} (for {user})"))
    });
    model
        .packages
        .iter()
        .cloned()
        .chain(users)
        .map(|name| (name, ()))
        .collect()
}

/// Files in users' homes, by path as `~user/.config/git/config`.
fn home_files(model: &Model) -> BTreeMap<String, File> {
    model
        .homes
        .iter()
        .flat_map(|(user, home)| {
            home.files
                .iter()
                .map(move |(path, file)| (home_path(user, path), file.clone()))
        })
        .collect()
}

/// Compares two maps by key; `describe` turns one difference into details and a tier.
fn diff<T: PartialEq>(
    before: &BTreeMap<String, T>,
    after: &BTreeMap<String, T>,
    describe: impl Fn(Option<&T>, Option<&T>) -> (Vec<String>, Option<Tier>),
) -> Vec<Change> {
    diff_with_names(before, after, |_, old, new| describe(old, new))
}

/// Like [`diff`], with the key passed to `describe` as well.
fn diff_with_names<T: PartialEq>(
    before: &BTreeMap<String, T>,
    after: &BTreeMap<String, T>,
    describe: impl Fn(&str, Option<&T>, Option<&T>) -> (Vec<String>, Option<Tier>),
) -> Vec<Change> {
    let names: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    names
        .into_iter()
        .filter_map(|name| {
            let (old, new) = (before.get(name), after.get(name));
            let kind = match (old, new) {
                (None, Some(_)) => ChangeKind::Added,
                (Some(_), None) => ChangeKind::Removed,
                (Some(old), Some(new)) if old != new => ChangeKind::Changed,
                _ => return None,
            };
            let (details, tier) = describe(name, old, new);
            Some(Change {
                kind,
                name: name.clone(),
                details,
                tier,
            })
        })
        .collect()
}

/// The unit entries that differ, as `- Section.Key=value` and `+ Section.Key=value` lines.
fn unit_details(before: Option<&Unit>, after: Option<&Unit>) -> Vec<String> {
    let (Some(before), Some(after)) = (before, after) else {
        return Vec::new();
    };
    let entries = |unit: &Unit| -> Vec<String> {
        unit.sections
            .iter()
            .flat_map(|section| {
                section
                    .entries
                    .iter()
                    .map(move |(key, value)| format!("{}.{key}={value}", section.name))
            })
            .chain(
                unit.restart_on
                    .iter()
                    .map(|path| format!("restarts on {path}")),
            )
            .collect()
    };
    let (old, new) = (entries(before), entries(after));
    let removed = old
        .iter()
        .filter(|entry| !new.contains(entry))
        .map(|entry| format!("- {entry}"));
    let added = new
        .iter()
        .filter(|entry| !old.contains(entry))
        .map(|entry| format!("+ {entry}"));
    removed.chain(added).collect()
}
