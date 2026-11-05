//! Comparing two models: what would change, and how disruptive applying it is.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use distro_eval::Section;

use crate::Model;

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
    pub units: Vec<Change>,
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
            ("units", self.units.as_slice()),
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
    Plan {
        options: diff(&before.options, &after.options, |before, after| {
            let detail = match (before, after) {
                (Some(before), Some(after)) => format!("{before} -> {after}"),
                (None, Some(value)) | (Some(value), None) => value.to_string(),
                (None, None) => unreachable!("one side has the option"),
            };
            (vec![detail], None)
        }),
        packages: diff(
            &as_map(&before.packages),
            &as_map(&after.packages),
            |_, _| (Vec::new(), Some(Tier::Live)),
        ),
        files: diff(&before.files, &after.files, |before, after| {
            let detail = match (before, after) {
                (Some(before), Some(after)) if before.mode != after.mode => {
                    vec![format!("mode {:o} -> {:o}", before.mode, after.mode)]
                }
                _ => Vec::new(),
            };
            (detail, Some(Tier::Live))
        }),
        units: diff(&before.units, &after.units, |before, after| {
            (unit_details(before, after), Some(Tier::Live))
        }),
    }
}

fn as_map(set: &BTreeSet<String>) -> BTreeMap<String, ()> {
    set.iter().map(|item| (item.clone(), ())).collect()
}

/// Compares two maps by key; `describe` turns one difference into details and a tier.
fn diff<T: PartialEq>(
    before: &BTreeMap<String, T>,
    after: &BTreeMap<String, T>,
    describe: impl Fn(Option<&T>, Option<&T>) -> (Vec<String>, Option<Tier>),
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
            let (details, tier) = describe(old, new);
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
fn unit_details(before: Option<&Vec<Section>>, after: Option<&Vec<Section>>) -> Vec<String> {
    let (Some(before), Some(after)) = (before, after) else {
        return Vec::new();
    };
    let entries = |sections: &Vec<Section>| -> Vec<String> {
        sections
            .iter()
            .flat_map(|section| {
                section
                    .entries
                    .iter()
                    .map(move |(key, value)| format!("{}.{key}={value}", section.name))
            })
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
