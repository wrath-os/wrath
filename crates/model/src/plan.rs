//! Comparing two models: what would change, and how disruptive applying it is.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use wrath_eval::{Template, Unit, UnitAction, Value, Wrapper, home_path};

use crate::{File, Model, Secret, Source, WRATH_SOURCE};

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

impl ChangeKind {
    /// How plans mark it: `+`, `-` or `~`.
    pub fn sign(self) -> char {
        match self {
            Self::Added => '+',
            Self::Removed => '-',
            Self::Changed => '~',
        }
    }
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
    /// What switching does to the system's units, from the same comparison as `units`.
    pub activation: Activation,
    /// Whether every file the plan changes is a setting: as the model before says for a removed
    /// file, and as the one after says for the rest, since that's what the file is now.
    pub files_are_settings: bool,
    /// Whether it plans a system from nothing, whose options are all new and shown as a count.
    pub fresh: bool,
}

/// The system's units a switch stops, starts, restarts or reloads, by name. Users' units are
/// their homes' to switch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activation {
    pub stop: BTreeSet<String>,
    pub start: BTreeSet<String>,
    pub restart: BTreeSet<String>,
    pub reload: BTreeSet<String>,
}

impl Activation {
    fn from_changes(units: &[Change], after: &Model, files: &[Change]) -> Self {
        let mut activation = Self::default();
        for change in units
            .iter()
            .filter(|change| !change.name.contains(" (for "))
        {
            // A drop-in such as `nginx.service.d/limits.conf` changes the unit it belongs to. One
            // for a template, such as `getty@.service.d/`, reaches its instances as they start.
            if let Some((unit, _)) = change.name.split_once(".d/") {
                if !is_template(unit) {
                    activation.restart.insert(unit.to_owned());
                }
                continue;
            }
            let name = change.name.clone();
            match change.kind {
                ChangeKind::Added => activation.start.insert(name),
                ChangeKind::Removed => activation.stop.insert(name),
                ChangeKind::Changed => activation.restart.insert(name),
            };
        }
        for ((unit, action), paths) in &after.triggers {
            if files
                .iter()
                .any(|change| paths.contains(change.name.as_str()))
            {
                match action {
                    UnitAction::Restart => activation.restart.insert(unit.clone()),
                    UnitAction::Reload => activation.reload.insert(unit.clone()),
                };
            }
        }
        activation.start.retain(|unit| !is_template(unit));
        activation.stop.retain(|unit| !is_template(unit));
        activation
            .restart
            .retain(|unit| !keeps_running(after, unit));
        activation
            .reload
            .retain(|unit| !keeps_running(after, unit) && !activation.restart.contains(unit));
        activation
    }

    /// Restarts `units` too, such as those whose programs changed though their files didn't,
    /// except what starts or stops anyway and what [`keeps_running`] leaves be.
    pub fn restart_too(&mut self, after: &Model, units: impl IntoIterator<Item = String>) {
        for unit in units {
            if !keeps_running(after, &unit)
                && !self.start.contains(&unit)
                && !self.stop.contains(&unit)
            {
                self.reload.remove(&unit);
                self.restart.insert(unit);
            }
        }
    }
}

/// Whether a switch leaves `unit` running as it is when it changes: a template, which reaches its
/// instances as they start, a mount or swap, which restarting would take away, and a unit whose
/// `[Unit]` says `X-RestartIfChanged=false`, in its file or a drop-in, such as a login manager
/// whose restart would end the sessions it started, or the message bus, which services lose for
/// good. Its changes take effect at the next boot.
fn keeps_running(after: &Model, unit: &str) -> bool {
    let drop_ins = format!("{unit}.d/");
    is_template(unit)
        || unit.ends_with(".mount")
        || unit.ends_with(".swap")
        || after
            .units
            .iter()
            .filter(|(name, _)| *name == unit || name.starts_with(&drop_ins))
            .flat_map(|(_, unit)| &unit.sections)
            .filter(|section| section.name == "Unit")
            .flat_map(|section| &section.entries)
            .any(|(key, value)| key == "X-RestartIfChanged" && value == "false")
}

fn is_template(unit: &str) -> bool {
    unit.contains("@.")
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.sections().all(|(_, changes)| changes.is_empty())
    }

    /// Whether the plan only changes settings, which watching applies without asking: files
    /// marked as settings, and no package, secret, unit, account or boot change, nor a service
    /// started, stopped, restarted or reloaded. Options change with them, and only matter for what
    /// they render.
    pub fn settings_only(&self) -> bool {
        self.files_are_settings
            && self.packages.is_empty()
            && self.secrets.is_empty()
            && self.units.is_empty()
            && self.base.is_empty()
            && self.activation == Activation::default()
    }

    /// The most disruptive tier any change needs.
    pub fn tier(&self) -> Option<Tier> {
        self.sections()
            .flat_map(|(_, changes)| changes.iter())
            .filter_map(|change| change.tier)
            .max()
    }

    /// The changes in one line, naming up to `limit` of them: `~ services.web.port, + jq`.
    /// Options and packages come first, since they're what people change; the rest only show
    /// when nothing like that changed.
    pub fn summary(&self, limit: usize) -> String {
        let mut changes: Vec<&Change> = self.options.iter().chain(&self.packages).collect();
        if changes.is_empty() {
            changes = self.sections().flat_map(|(_, changes)| changes).collect();
        }
        if changes.is_empty() {
            return "no changes".to_owned();
        }
        let named: Vec<String> = changes
            .iter()
            .take(limit)
            .map(|change| format!("{} {}", change.kind.sign(), change.name))
            .collect();
        match changes.len().saturating_sub(limit) {
            0 => named.join(", "),
            more => format!("{} and {more} more", named.join(", ")),
        }
    }

    /// The plan as data, for `--json`: every change with its section, and the overall tier.
    pub fn to_value(&self) -> Value {
        let changes = self
            .sections()
            .flat_map(|(section, changes)| changes.iter().map(move |change| (section, change)))
            .map(|(section, change)| {
                let kind = match change.kind {
                    ChangeKind::Added => "added",
                    ChangeKind::Removed => "removed",
                    ChangeKind::Changed => "changed",
                };
                let mut entry = BTreeMap::from([
                    ("section".to_owned(), Value::Str(section.to_owned())),
                    ("kind".to_owned(), Value::Str(kind.to_owned())),
                    ("name".to_owned(), Value::Str(change.name.clone())),
                    (
                        "details".to_owned(),
                        Value::List(change.details.iter().cloned().map(Value::Str).collect()),
                    ),
                ]);
                if let Some(tier) = change.tier {
                    entry.insert("tier".to_owned(), Value::Str(tier.to_string()));
                }
                Value::Table(entry)
            })
            .collect();
        let mut plan = BTreeMap::from([("changes".to_owned(), Value::List(changes))]);
        if let Some(tier) = self.tier() {
            plan.insert("tier".to_owned(), Value::Str(tier.to_string()));
        }
        Value::Table(plan)
    }

    fn sections(&self) -> impl Iterator<Item = (&'static str, &[Change])> {
        [
            ("options", self.options.as_slice()),
            ("packages", self.packages.as_slice()),
            ("files", self.files.as_slice()),
            ("secrets", self.secrets.as_slice()),
            ("units", self.units.as_slice()),
            ("base", self.base.as_slice()),
        ]
        .into_iter()
    }
}

impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write(f, false)
    }
}

impl Plan {
    /// The plan as text, with color for a terminal when `color` says so: what's added, removed
    /// and changed in green, red and yellow, details dimmed, and tiers that need more than a
    /// switch stand out.
    pub fn styled(&self, color: bool) -> String {
        let mut out = String::new();
        self.write(&mut out, color)
            .expect("writing to a String doesn't fail");
        out
    }

    fn write(&self, f: &mut impl fmt::Write, color: bool) -> fmt::Result {
        let paint = |codes: &str, text: &str| {
            if color {
                format!("\x1b[{codes}m{text}\x1b[0m")
            } else {
                text.to_owned()
            }
        };
        let tier = |tier: Tier| {
            let codes = match tier {
                Tier::Live => "2",
                Tier::NextLogin | Tier::SoftReboot => "1;33",
                Tier::Reboot => "1;31",
            };
            paint(codes, &tier.to_string())
        };
        if self.is_empty() {
            return writeln!(f, "no changes");
        }
        let mut count = 0;
        for (heading, changes) in self.sections().filter(|(_, changes)| !changes.is_empty()) {
            writeln!(f, "{}", paint("1", heading))?;
            // Every option is new in a system from nothing; listing them all would bury the
            // packages, files and units the plan is read for.
            if self.fresh && heading == "options" {
                count += changes.len();
                let line = format!(
                    "+ {} options; `wrath why` shows where each is set",
                    changes.len()
                );
                writeln!(f, "  {}", paint("32", &line))?;
                continue;
            }
            for change in changes {
                count += 1;
                let kind = match change.kind {
                    ChangeKind::Added => "32",
                    ChangeKind::Removed => "31",
                    ChangeKind::Changed => "33",
                };
                let line = format!("{} {}", change.kind.sign(), change.name);
                write!(f, "  {}", paint(kind, &line))?;
                if let Some(at) = change.tier {
                    write!(
                        f,
                        "{}{}{}",
                        paint("2", " (tier: "),
                        tier(at),
                        paint("2", ")")
                    )?;
                }
                match change.details.as_slice() {
                    [detail] if change.tier.is_none() => writeln!(f, ": {detail}")?,
                    details => {
                        writeln!(f)?;
                        for detail in details {
                            writeln!(f, "      {}", paint("2", detail))?;
                        }
                    }
                }
            }
        }
        let noun = if count == 1 { "change" } else { "changes" };
        match self.tier() {
            Some(at) => writeln!(f, "{count} {noun}, tier: {}", tier(at)),
            None => writeln!(f, "{count} {noun}, nothing to apply"),
        }
    }
}

/// What changes going from `before` to `after`.
pub fn plan(before: &Model, after: &Model) -> Plan {
    // A file only read at login changes for a user when they log in again.
    let file_details = |path: &str, old: Option<&File>, new: Option<&File>| {
        let detail = match (old, new) {
            (Some(old), Some(new)) if old.mode != new.mode => {
                vec![format!("mode {:o} -> {:o}", old.mode, new.mode)]
            }
            _ => Vec::new(),
        };
        let tier = if before.next_login.contains(path) || after.next_login.contains(path) {
            Tier::NextLogin
        } else {
            Tier::Live
        };
        (detail, Some(tier))
    };
    let mut files = diff_with_names(&before.files, &after.files, file_details);
    files.extend(diff(&wrappers(before), &wrappers(after), |old, new| {
        let detail = |wrapper: &Wrapper| {
            format!(
                "{}:{} mode {:o}, of {}",
                wrapper.owner, wrapper.group, wrapper.mode, wrapper.source
            )
        };
        let details = match (old, new) {
            (Some(old), Some(new)) => vec![format!("{} -> {}", detail(old), detail(new))],
            (None, Some(wrapper)) | (Some(wrapper), None) => vec![detail(wrapper)],
            (None, None) => Vec::new(),
        };
        (details, Some(Tier::Live))
    }));
    files.extend(diff_with_names(
        &home_files(before),
        &home_files(after),
        file_details,
    ));
    files.extend(diff(
        &templates(before),
        &templates(after),
        |before, after| {
            (
                template_details(before.copied(), after.copied()),
                Some(Tier::Live),
            )
        },
    ));
    let mut units = diff(&units(before), &units(after), |before, after| {
        (
            unit_details(before.copied(), after.copied()),
            Some(Tier::Live),
        )
    });
    units.extend(diff(&enabled(before), &enabled(after), |_, after| {
        let detail = match after {
            Some(targets) if !targets.is_empty() => {
                let targets: Vec<&str> = targets.iter().map(String::as_str).collect();
                format!("wanted by {}", targets.join(", "))
            }
            _ => "enabled".to_owned(),
        };
        (vec![detail], Some(Tier::Live))
    }));
    let secrets = diff(&before.secrets, &after.secrets, |before, after| {
        (secret_details(before, after), Some(Tier::Live))
    });
    for change in &mut units {
        // A switch starts and stops users' units, but restarting one could end their session.
        if change.kind == ChangeKind::Changed && change.name.contains(" (for ") {
            change.tier = Some(Tier::NextLogin);
            change
                .details
                .push("running sessions keep the old one until the next login".to_owned());
        }
    }
    units.extend(restarts(before, after, &files, &secrets));
    let activation = Activation::from_changes(&units, after, &files);
    // A unit the plan has already starts, stops or restarts, which a reload adds nothing to. A
    // system from nothing starts everything, so nothing in it restarts.
    let fresh = before.files.is_empty() && before.units.is_empty();
    if !fresh {
        let shown: BTreeSet<String> = units.iter().map(|unit| unit.name.clone()).collect();
        units.extend(
            triggered(after, &files)
                .into_iter()
                .filter(|unit| !shown.contains(&unit.name)),
        );
    }
    units.sort_by(|a, b| a.name.cmp(&b.name));
    let (was, is) = (&before.setting_files, &after.setting_files);
    let files_are_settings = files.iter().all(|change| match change.kind {
        ChangeKind::Removed => was.contains(&change.name),
        ChangeKind::Added | ChangeKind::Changed => is.contains(&change.name),
    });
    Plan {
        files_are_settings,
        fresh,
        options: diff_with_names(&before.options, &after.options, |name, before, after| {
            // A password hash is as good as the password to anyone with time; plans don't show it.
            let detail = if name.ends_with("hashed_password") {
                "a password hash".to_owned()
            } else {
                value_change(before, after)
            };
            (vec![detail], None)
        }),
        packages: diff_with_names(&before.sources, &after.sources, source_change)
            .into_iter()
            .map(|change| Change {
                name: format!("source {}", change.name),
                ..change
            })
            .chain(builds(before, after))
            .chain(diff(&packages(before), &packages(after), |_, _| {
                (Vec::new(), Some(Tier::Live))
            }))
            .collect(),
        files,
        secrets,
        units,
        activation,
        base: diff_with_names(&before.base, &after.base, |name, before, after| {
            // The bootloader and kernel only change on the next boot.
            let tier = if name == "boot" || name == "kernel" {
                Tier::Reboot
            } else {
                Tier::Live
            };
            (vec![value_change(before, after)], Some(tier))
        })
        .into_iter()
        .chain(platform(before, after))
        .collect(),
    }
}

/// The platform the system builds on, when it changed. The modules it brings and drops show as
/// the changes they make.
fn platform(before: &Model, after: &Model) -> Option<Change> {
    let name = |model: &Model| model.platform.clone().unwrap_or_else(|| "wrath".to_owned());
    (before.platform != after.platform).then(|| Change {
        kind: ChangeKind::Changed,
        name: "platform".to_owned(),
        details: vec![format!("{} -> {}", name(before), name(after))],
        tier: Some(Tier::Live),
    })
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
        .map(|shown| (shown, ()))
        .collect()
}

/// How a file with secrets in it changed, or who owns a new one.
fn template_details(before: Option<&Template>, after: Option<&Template>) -> Vec<String> {
    let owner = |template: &Template| match &template.group {
        Some(group) => format!("{}:{group}", template.owner),
        None => template.owner.clone(),
    };
    match (before, after) {
        (None, Some(new)) => vec![format!(
            "with secrets, filled in on the machine for {}, mode {:o}",
            owner(new),
            new.mode
        )],
        (Some(old), Some(new)) => {
            let mut details = Vec::new();
            if old.parts != new.parts {
                details.push("text changed".to_owned());
            }
            if owner(old) != owner(new) {
                details.push(format!("owner {} -> {}", owner(old), owner(new)));
            }
            if old.mode != new.mode {
                details.push(format!("mode {:o} -> {:o}", old.mode, new.mode));
            }
            details
        }
        _ => Vec::new(),
    }
}

/// How the packages built differently changed.
fn builds(before: &Model, after: &Model) -> Vec<Change> {
    diff(&before.builds, &after.builds, |old, new| {
        let details = match (old, new) {
            (Some(old), Some(new)) => {
                let parts = [
                    (
                        old.package != new.package,
                        format!("{} -> {}", old.package, new.package),
                    ),
                    (old.options != new.options, "options changed".to_owned()),
                    (old.patches != new.patches, "patches changed".to_owned()),
                ];
                parts
                    .into_iter()
                    .filter_map(|(changed, part)| changed.then_some(part))
                    .collect()
            }
            (_, Some(build)) | (Some(build), _) => vec![format!("from {}", build.package)],
            (None, None) => Vec::new(),
        };
        (details, Some(Tier::Live))
    })
    .into_iter()
    .map(|change| Change {
        name: format!("build {}", change.name),
        ..change
    })
    .collect()
}

/// How a package source changed. The base system comes from the default source too, and a
/// switch can't restart all of it, so moving that source to another locked version needs a soft
/// reboot. Locking a source for the first time pins what's already running.
fn source_change(
    name: &str,
    before: Option<&Source>,
    after: Option<&Source>,
) -> (Vec<String>, Option<Tier>) {
    let version = |source: &Source| match &source.rev {
        Some(rev) => rev[..rev.len().min(12)].to_owned(),
        None if source.is_locked() => source.hash.clone(),
        None => "its newest version".to_owned(),
    };
    let describe = |source: &Source| format!("{} at {}", source.url, version(source));
    let detail = match (before, after) {
        (Some(old), Some(new)) if old.url == new.url => {
            format!("{}: {} -> {}", new.url, version(old), version(new))
        }
        (Some(old), Some(new)) => format!("{} -> {}", describe(old), describe(new)),
        (None, Some(source)) | (Some(source), None) => describe(source),
        (None, None) => String::new(),
    };
    let moved = before
        .zip(after)
        .is_some_and(|(old, new)| old.is_locked() && new.is_locked());
    if moved && name == WRATH_SOURCE {
        let reboot = "the base system, such as systemd and D-Bus, runs fully after a soft reboot";
        (vec![detail, reboot.to_owned()], Some(Tier::SoftReboot))
    } else {
        (vec![detail], Some(Tier::Live))
    }
}

/// The system's units, and each user's as `name (for user)`.
fn units(model: &Model) -> BTreeMap<String, &Unit> {
    let users = model.homes.iter().flat_map(|(user, home)| {
        home.units
            .iter()
            .map(move |(name, unit)| (format!("{name} (for {user})"), unit))
    });
    model
        .units
        .iter()
        .map(|(name, unit)| (name.clone(), unit))
        .chain(users)
        .collect()
}

/// Programs in `/run/wrath/wrappers/bin`, by path.
fn wrappers(model: &Model) -> BTreeMap<String, &Wrapper> {
    model
        .wrappers
        .iter()
        .map(|(name, wrapper)| (format!("/run/wrath/wrappers/bin/{name}"), wrapper))
        .collect()
}

/// The units the service packages ship that the system enables, and the user units every user's
/// service manager does.
fn enabled(model: &Model) -> BTreeMap<String, &BTreeSet<String>> {
    let users = model
        .enabled_user
        .iter()
        .map(|(unit, targets)| (format!("{unit} (for every user)"), targets));
    model
        .enabled
        .iter()
        .map(|(unit, targets)| (unit.clone(), targets))
        .chain(users)
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

/// Files with secrets in them, the system's and each user's as `~user/.netrc`.
fn templates(model: &Model) -> BTreeMap<String, &Template> {
    let users = model.homes.iter().flat_map(|(user, home)| {
        home.templates
            .iter()
            .map(move |(path, template)| (home_path(user, path), template))
    });
    model
        .templates
        .iter()
        .map(|(path, template)| (path.clone(), template))
        .chain(users)
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

/// The packages a newer wrath rebuilds: those `before`, the keys a system was built from, has
/// with another key in `after`, today's recipes. The kernel, its microcode and firmware take a
/// reboot, what the base system runs takes a soft reboot, and the rest is live.
pub fn rebuilt(
    model: &Model,
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Vec<Change> {
    fn field<'a>(table: Option<&'a Value>, key: &str) -> Option<&'a Value> {
        match table {
            Some(Value::Table(table)) => table.get(key),
            _ => None,
        }
    }
    let kernel = model.base.get("kernel");
    let firmware = match field(kernel, "firmware") {
        Some(Value::List(ids)) => ids.as_slice(),
        _ => &[],
    };
    let boots: Vec<&str> = [
        field(kernel, "package"),
        field(field(kernel, "microcode"), "package"),
    ]
    .into_iter()
    .flatten()
    .chain(firmware)
    .filter_map(|id| match id {
        Value::Str(id) => Some(id.as_str()),
        _ => None,
    })
    .collect();
    before
        .iter()
        .filter(|(id, key)| after.get(*id).is_some_and(|now| now != *key))
        .map(|(id, _)| {
            let tier = if boots.contains(&id.as_str()) {
                Tier::Reboot
            } else if BASE_SERVICES.contains(&id.as_str()) {
                Tier::SoftReboot
            } else {
                Tier::Live
            };
            Change {
                kind: ChangeKind::Changed,
                name: id.clone(),
                details: vec!["rebuilt".to_owned()],
                tier: Some(tier),
            }
        })
        .collect()
}

/// What the base system runs from boot to shutdown, which a switch can't restart.
const BASE_SERVICES: [&str; 2] = ["systemd", "dbus"];
