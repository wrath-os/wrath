//! Step 15: a distro model lowered into actions, each with two keys. An unresolved key names what
//! an action depends on by those actions' keys, and is known before anything runs. A resolved key
//! names what they really produced. Where an action came from is kept beside it, unhashed.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::chain::{self, Arm, Built};
use crate::early;
use crate::initrd;
use crate::pilot;
use crate::store::{Lab, StorePath, hex, with_path};

fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))[..40].to_owned()
}

/// serde_json keeps object keys sorted, so a record's text is canonical.
fn key_of(record: &Value) -> String {
    digest(record.to_string().as_bytes())
}

fn sql(error: rusqlite::Error) -> String {
    format!("sqlite: {error}")
}

/// Records, outputs and where actions came from.
pub struct Db(Connection);

impl Db {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(with_path(parent))?;
        }
        let connection = Connection::open(path).map_err(sql)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(30))
            .map_err(sql)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=NORMAL;
                 CREATE TABLE IF NOT EXISTS actions(key TEXT PRIMARY KEY, record TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS outputs(key TEXT PRIMARY KEY, output TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS parts(generation TEXT, path TEXT, entry TEXT,
                     PRIMARY KEY(generation, path));
                 CREATE TABLE IF NOT EXISTS applies(
                     seq INTEGER PRIMARY KEY AUTOINCREMENT, time INTEGER NOT NULL,
                     who TEXT NOT NULL, kind TEXT NOT NULL, sources TEXT NOT NULL,
                     lock TEXT NOT NULL, explanation TEXT NOT NULL, model TEXT NOT NULL,
                     generation TEXT NOT NULL, previous INTEGER);",
            )
            .map_err(sql)?;
        Ok(Self(connection))
    }
}

fn insert_pair(tx: &Transaction, statement: &str, key: &str, value: &str) -> Result<(), String> {
    tx.prepare_cached(statement)
        .and_then(|mut s| s.execute(params![key, value]))
        .map(|_| ())
        .map_err(sql)
}

fn put_action(tx: &Transaction, key: &str, record: &Value) -> Result<(), String> {
    let statement = "INSERT OR IGNORE INTO actions VALUES (?1, ?2)";
    insert_pair(tx, statement, key, &record.to_string())
}

fn put_output(tx: &Transaction, key: &str, output: &str) -> Result<(), String> {
    insert_pair(
        tx,
        "INSERT OR IGNORE INTO outputs VALUES (?1, ?2)",
        key,
        output,
    )
}

fn output_of(tx: &Transaction, key: &str) -> Result<Option<String>, String> {
    tx.prepare_cached("SELECT output FROM outputs WHERE key = ?1")
        .and_then(|mut s| s.query_row([key], |row| row.get(0)).optional())
        .map_err(sql)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Entry {
    File { mode: u32, object: String },
    Link { target: String },
}

type Entries = BTreeMap<String, Entry>;

/// A package a model can use, as the graph knows it.
pub struct PackageRef {
    pub path: StorePath,
    pub unresolved: String,
}

pub type Packages = BTreeMap<String, PackageRef>;

struct Objects(PathBuf);

impl Objects {
    fn put(&self, bytes: &[u8]) -> Result<String, String> {
        let hash = digest(bytes);
        let path = self.0.join(&hash);
        if !path.exists() {
            fs::write(&path, bytes).map_err(with_path(&path))?;
        }
        Ok(hash)
    }
}

type Run<'a> = Box<dyn Fn(&Objects) -> Result<Entries, String> + 'a>;

struct Action<'a> {
    origin: String,
    unresolved: Value,
    resolved: Value,
    run: Run<'a>,
}

/// An action's unresolved and resolved records: `record` given `ids` by unresolved key, then by
/// real path.
fn action_records(
    ids: &[&str],
    packages: &Packages,
    record: impl Fn(Value) -> Value,
) -> Result<(Value, Value), String> {
    let (mut unresolved, mut resolved) = (Map::new(), Map::new());
    for id in ids {
        let package = package(packages, id)?;
        unresolved.insert((*id).to_owned(), json!(package.unresolved));
        resolved.insert((*id).to_owned(), json!(package.path.hash()));
    }
    Ok((
        record(Value::Object(unresolved)),
        record(Value::Object(resolved)),
    ))
}

fn package<'a>(packages: &'a Packages, id: &str) -> Result<&'a PackageRef, String> {
    packages
        .get(id)
        .ok_or_else(|| format!("no recipe for package `{id}`"))
}

/// `template` with `@id@` replaced by package `id`'s path, for each of `ids`.
fn fill(template: &str, ids: &[impl AsRef<str>], packages: &Packages) -> Result<String, String> {
    let mut filled = template.to_owned();
    for id in ids {
        let id = id.as_ref();
        filled = filled.replace(&format!("@{id}@"), &package(packages, id)?.path.inside());
    }
    Ok(filled)
}

/// A file from a template, where `@id@` stands for package `id`'s path.
fn text<'a>(
    origin: &str,
    path: &str,
    mode: u32,
    template: String,
    ids: &[&str],
    packages: &'a Packages,
) -> Result<Action<'a>, String> {
    let (unresolved, resolved) = action_records(ids, packages, |deps| {
        json!({"kind": "text", "path": path, "mode": mode,
            "template": template, "deps": deps})
    })?;
    let path = path.to_owned();
    let names: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
    Ok(Action {
        origin: origin.to_owned(),
        unresolved,
        resolved,
        run: Box::new(move |objects| {
            let filled = fill(&template, &names, packages)?;
            let object = objects.put(filled.as_bytes())?;
            Ok(BTreeMap::from([(
                path.clone(),
                Entry::File { mode, object },
            )]))
        }),
    })
}

/// Links in `to` for every file in `dir` of `package`, leaving entries already there alone.
fn linked(
    lab: &Lab,
    package: &PackageRef,
    dir: &str,
    to: &str,
    entries: &mut Entries,
) -> Result<(), String> {
    let source = lab.on_host(&package.path).join(dir);
    let Ok(listing) = fs::read_dir(&source) else {
        return Ok(());
    };
    let mut names: Vec<_> = listing
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        entries
            .entry(format!("{to}/{name}"))
            .or_insert(Entry::Link {
                target: format!("{}/{dir}/{name}", package.path.inside()),
            });
    }
    Ok(())
}

/// An action that makes links into packages. `layout` lists directories as (from, to).
fn links<'a>(
    origin: &str,
    lab: &'a Lab,
    ids: Vec<&'a str>,
    layout: Vec<(&'static str, &'static str)>,
    packages: &'a Packages,
) -> Result<Action<'a>, String> {
    let (unresolved, resolved) = action_records(
        &ids,
        packages,
        |deps| json!({"kind": "links", "layout": layout, "deps": deps}),
    )?;
    Ok(Action {
        origin: origin.to_owned(),
        unresolved,
        resolved,
        run: Box::new(move |_| {
            let mut entries = Entries::new();
            for id in &ids {
                for (from, to) in &layout {
                    linked(lab, package(packages, id)?, from, to, &mut entries)?;
                }
            }
            Ok(entries)
        }),
    })
}

const BASE_PACKAGES: &[&str] = &[
    "bash",
    "bc",
    "coreutils",
    "dbus",
    "diffutils",
    "distro",
    "findutils",
    "gawk",
    "grep",
    "kmod",
    "sed",
    "systemd",
    "util-linux",
];

fn system_layout() -> Vec<(&'static str, &'static str)> {
    vec![
        ("lib/sysusers.d", "etc/sysusers.d"),
        ("lib/tmpfiles.d", "etc/tmpfiles.d"),
        ("lib/sysctl.d", "etc/sysctl.d"),
        ("lib/binfmt.d", "etc/binfmt.d"),
        ("lib/modules-load.d", "etc/modules-load.d"),
        ("lib/udev/rules.d", "etc/udev/rules.d"),
        ("lib/udev/hwdb.d", "etc/udev/hwdb.d"),
        ("share/dbus-1/system.d", "etc/dbus-1/system.d"),
    ]
}

/// `etc/...` for a file under `/etc`, otherwise `files/...`, which a tmpfiles rule links in.
fn installed_at(path: &str) -> String {
    match path.strip_prefix("/etc/") {
        Some(rest) => format!("etc/{rest}"),
        None => format!("files{path}"),
    }
}

fn str_field<'a>(value: &'a Value, what: &str) -> Result<&'a str, String> {
    value
        .as_str()
        .ok_or_else(|| format!("the model's {what} isn't text"))
}

fn account_files(model: &Value) -> Result<(String, String, String), String> {
    let mut passwd =
        String::from("root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534:nobody:/:/bin/sh\n");
    let mut shadow = String::from("root:*:1::::::\nnobody:*:1::::::\n");
    let mut members: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let users = model["base"]["users"].as_object().into_iter().flatten();
    for (index, (name, user)) in users.enumerate() {
        for extra in user["extra_groups"].as_array().into_iter().flatten() {
            let extra = str_field(extra, "extra group")?;
            members.entry(extra).or_default().push(name);
        }
        let (home, description) = (
            str_field(&user["home"], "user's home")?,
            user["description"].as_str().unwrap_or(""),
        );
        let uid = 1000 + index;
        let shell = user["shell"]
            .as_str()
            .unwrap_or("/run/current-system/sw/bin/bash");
        passwd.push_str(&format!(
            "{name}:x:{uid}:100:{description}:{home}:{shell}\n"
        ));
        let hash = user["initial_hashed_password"].as_str().unwrap_or("!");
        shadow.push_str(&format!("{name}:{hash}:1::::::\n"));
    }
    let wheel = members.remove("wheel").unwrap_or_default().join(",");
    let mut group = format!("root:x:0:\nwheel:x:10:{wheel}\nnogroup:x:65534:\nusers:x:100:\n");
    for (gid, (name, names)) in (400..).zip(members) {
        group.push_str(&format!("{name}:x:{gid}:{}\n", names.join(",")));
    }
    Ok((passwd, group, shadow))
}

/// The kernel's options for the system's boot entries: where its root is, then the model's.
fn kernel_params(model: &Value) -> Result<String, String> {
    let mut params = vec![format!("root=LABEL={ROOT_LABEL}"), "rw".to_owned()];
    for param in model["base"]["boot"]["kernel_params"]
        .as_array()
        .into_iter()
        .flatten()
    {
        params.push(str_field(param, "kernel option")?.to_owned());
    }
    Ok(params.join(" ") + "\n")
}

/// tmpfiles rules that make each user's home, owned by them, when it doesn't exist.
fn homes(model: &Value) -> Result<String, String> {
    let mut rules = String::new();
    for (name, user) in model["base"]["users"].as_object().into_iter().flatten() {
        let home = str_field(&user["home"], "user's home")?;
        rules.push_str(&format!("d {home} 0700 {name} users -\n"));
    }
    Ok(rules)
}

fn render_unit(sections: &Value, packages: &[(PathBuf, String)]) -> Result<String, String> {
    let mut out = String::new();
    for section in sections.as_array().ok_or("a unit has no sections")? {
        out.push_str(&format!("[{}]\n", str_field(&section["name"], "section")?));
        for entry in section["entries"]
            .as_array()
            .ok_or("a section has no entries")?
        {
            let key = str_field(&entry[0], "key")?;
            let mut value = str_field(&entry[1], "value")?.to_owned();
            if key.starts_with("Exec") {
                // A command may start with prefixes such as `-` (failure is fine).
                let command = value.trim_start_matches(['-', '@', ':', '+', '!']);
                let (prefixes, command) = value.split_at(value.len() - command.len());
                let program = command.split(' ').next().unwrap_or("");
                if !program.is_empty()
                    && let Some((_, inside)) = packages
                        .iter()
                        .find(|(host, _)| host.join("bin").join(program).exists())
                {
                    value = format!("{prefixes}{inside}/bin/{command}");
                }
            }
            out.push_str(&format!("{key}={value}\n"));
        }
        out.push('\n');
    }
    Ok(out)
}

fn unit_action<'a>(
    lab: &'a Lab,
    name: &str,
    unit: &'a Value,
    ids: Vec<&'a str>,
    packages: &'a Packages,
) -> Result<Action<'a>, String> {
    let sections = &unit["sections"];
    let (unresolved, resolved) = action_records(
        &ids,
        packages,
        |deps| json!({"kind": "unit", "name": name, "sections": sections, "deps": deps}),
    )?;
    let name = name.to_owned();
    Ok(Action {
        origin: format!("units.{name}"),
        unresolved,
        resolved,
        run: Box::new(move |objects| {
            let mut found = Vec::new();
            for id in &ids {
                let path = &package(packages, id)?.path;
                found.push((lab.on_host(path), path.inside()));
            }
            let rendered = render_unit(sections, &found)?;
            let mut entries = BTreeMap::from([(
                format!("etc/systemd/system/{name}"),
                Entry::File {
                    mode: 0o644,
                    object: objects.put(rendered.as_bytes())?,
                },
            )]);
            for section in sections.as_array().into_iter().flatten() {
                if section["name"] != "Install" {
                    continue;
                }
                for entry in section["entries"].as_array().into_iter().flatten() {
                    if entry[0] == "WantedBy" {
                        let target = str_field(&entry[1], "target")?;
                        entries.insert(
                            format!("etc/systemd/system/{target}.wants/{name}"),
                            Entry::Link {
                                target: format!("../{name}"),
                            },
                        );
                    }
                }
            }
            Ok(entries)
        }),
    })
}

const INIT: &str = include_str!("../system/init");
const INITRD_ROOT: &str = include_str!("../system/initrd-root");
const SWITCH_TO: &str = include_str!("../system/switch-to-configuration");
const REPORT: &str = include_str!("../system/report");
const REPORT_UNIT: &str = "[Unit]\nDescription=Report how the boot went\n\
    After=multi-user.target\n\n\
    [Service]\nType=exec\nExecStart=/system/current/report\nStandardOutput=tty\n\
    StandardError=tty\nTTYPath=/dev/ttyS0\n";

/// The label of the file system a system's root is on.
pub const ROOT_LABEL: &str = "distro-root";

/// What a generation keeps of its model, so distro on the machine knows what it runs: distro's
/// canonical text of it, and that text's hash.
pub struct Kept {
    pub text: String,
    pub hash: String,
}

impl Kept {
    /// For a model that didn't come from distro, such as one `lab` reads from a file.
    pub fn plain(model: &Value) -> Self {
        let text = format!("{model}\n");
        Self {
            hash: digest(text.as_bytes()),
            text,
        }
    }
}

/// What distro on the machine reads at every boot and switch: files under /etc with a mode of
/// their own, which are copied instead of linked, and the work it does itself, such as secrets
/// and homes, which come later.
fn activation(model: &Value) -> String {
    let mut modes = Map::new();
    for (path, file) in model["files"].as_object().into_iter().flatten() {
        let mode = file["mode"].as_u64().unwrap_or(0o644);
        if let Some(inside) = path.strip_prefix("/etc/")
            && mode != 0o644
        {
            modes.insert(inside.to_owned(), json!(format!("{mode:04o}")));
        }
    }
    json!({
        "etc_modes": modes,
        "bash": "@bash@/bin/bash",
        "secrets": {},
        "templates": [],
        "homes": [],
    })
    .to_string()
}

/// Turns a model into actions. Every package it names must have a recipe.
fn lower<'a>(
    lab: &'a Lab,
    model: &'a Value,
    kept: &Kept,
    packages: &'a Packages,
) -> Result<Lowered<'a>, String> {
    let (named, skipped) = model_packages(model, |id| packages.contains_key(id))?;
    let everything: Vec<&'a str> = {
        let mut all: BTreeSet<&'a str> = BASE_PACKAGES.iter().copied().collect();
        all.extend(named.iter().copied());
        all.into_iter().collect()
    };

    let base = "base";
    let hostname = str_field(&model["base"]["system"]["hostname"], "hostname")?;
    let (passwd, group, shadow) = account_files(model)?;
    let mut actions = vec![
        text(
            base,
            "etc/os-release",
            0o644,
            "NAME=distro\nID=distro\n".into(),
            &[],
            packages,
        )?,
        text(
            base,
            "etc/hostname",
            0o644,
            format!("{hostname}\n"),
            &[],
            packages,
        )?,
        text(base, "etc/machine-id", 0o644, String::new(), &[], packages)?,
        text("base.users", "etc/passwd", 0o644, passwd, &[], packages)?,
        text("base.users", "etc/group", 0o644, group, &[], packages)?,
        text("base.users", "etc/shadow", 0o600, shadow, &[], packages)?,
        text(
            "base.users",
            "etc/tmpfiles.d/distro-homes.conf",
            0o644,
            homes(model)?,
            &[],
            packages,
        )?,
        text(
            base,
            "etc/dbus-1/system.d/services.conf",
            0o644,
            "<busconfig><servicedir>@systemd@/share/dbus-1/system-services</servicedir>\
             </busconfig>\n"
                .into(),
            &["systemd"],
            packages,
        )?,
        text(
            base,
            "etc/systemd/system.conf",
            0o644,
            "[Manager]\nDefaultEnvironment=PATH=/system/current/sw/bin\n".into(),
            &[],
            packages,
        )?,
        text(
            base,
            "init",
            0o755,
            INIT.into(),
            &["bash", "coreutils"],
            packages,
        )?,
        text(
            base,
            "bin/switch-to-configuration",
            0o755,
            SWITCH_TO.into(),
            &["bash", "coreutils"],
            packages,
        )?,
        text(
            "model",
            "etc/distro/model.json",
            0o644,
            kept.text.clone(),
            &[],
            packages,
        )?,
        text(
            "model",
            "etc/distro/model-hash",
            0o644,
            kept.hash.clone(),
            &[],
            packages,
        )?,
        text(
            "files",
            "activation.json",
            0o644,
            activation(model),
            &["bash"],
            packages,
        )?,
        text(
            "base.boot",
            "kernel-params",
            0o644,
            kernel_params(model)?,
            &[],
            packages,
        )?,
        links(
            "packages",
            lab,
            everything.clone(),
            vec![("bin", "sw/bin")],
            packages,
        )?,
        links(
            base,
            lab,
            vec!["systemd", "dbus"],
            system_layout(),
            packages,
        )?,
        links(
            "base.dbus",
            lab,
            vec!["dbus"],
            vec![("lib/systemd/system", "etc/systemd/system")],
            packages,
        )?,
        fixed_links(packages)?,
    ];

    let mut outside = String::new();
    for (path, file) in model["files"].as_object().ok_or("the model has no files")? {
        let at = installed_at(path);
        if at.starts_with("files/") {
            outside.push_str(&format!("L+ {path} - - - - /system/current/{at}\n"));
        }
        let mode = u32::try_from(file["mode"].as_u64().unwrap_or(0o644)).unwrap_or(0o644);
        actions.push(text(
            &format!("files.{path}"),
            &at,
            mode,
            str_field(&file["text"], "file text")?.to_owned(),
            &[],
            packages,
        )?);
    }
    actions.push(text(
        "files",
        "etc/tmpfiles.d/distro-files.conf",
        0o644,
        outside,
        &[],
        packages,
    )?);
    for (name, unit) in model["units"].as_object().ok_or("the model has no units")? {
        actions.push(unit_action(lab, name, unit, named.clone(), packages)?);
    }
    let (enabled, unfound) = enabled_units(lab, model, &everything, packages)?;
    actions.push(enabled);
    actions.push(boot_files(lab, model, packages)?);
    Ok(Lowered {
        actions,
        skipped,
        unfound,
    })
}

/// Links that pull in the model's enabled units: into each target it names, or into those its
/// `[Install]` section names when it names none. Also returns the units no package has.
fn enabled_units<'a>(
    lab: &'a Lab,
    model: &'a Value,
    present: &[&'a str],
    packages: &'a Packages,
) -> Result<(Action<'a>, Vec<String>), String> {
    // systemd's and dbus's own units first, then those of every other package the system has.
    let base = ["systemd", "dbus"];
    let mut ids: Vec<&'a str> = base.to_vec();
    ids.extend(present.iter().filter(|id| !base.contains(id)));
    let mut links = BTreeMap::new();
    let mut missing = Vec::new();
    for (unit, targets) in model["enabled"].as_object().into_iter().flatten() {
        let file = format!("lib/systemd/system/{unit}");
        let found = ids.iter().find_map(|id| {
            let path = &packages.get(*id)?.path;
            let host = lab.on_host(path).join(&file);
            host.is_file()
                .then(|| (*id, host, format!("{}/{file}", path.inside())))
        });
        let Some((id, host, inside)) = found else {
            missing.push(unit.clone());
            continue;
        };
        let mut wanted: Vec<String> = targets
            .as_array()
            .into_iter()
            .flatten()
            .map(|target| str_field(target, "target").map(str::to_owned))
            .collect::<Result<_, _>>()?;
        if wanted.is_empty() {
            let text = fs::read_to_string(&host).map_err(with_path(&host))?;
            wanted = installed_into(&text);
        }
        for target in wanted {
            links.insert(
                format!("etc/systemd/system/{target}.wants/{unit}"),
                inside.clone(),
            );
        }
        // systemd finds a wanted unit by its name on its search path, which has its own units and
        // dbus's already.
        if !base.contains(&id) {
            links.insert(format!("etc/systemd/system/{unit}"), inside);
        }
    }
    let (unresolved, resolved) = action_records(
        &ids,
        packages,
        |deps| json!({"kind": "enabled", "links": links, "deps": deps}),
    )?;
    let action = Action {
        origin: "enabled".to_owned(),
        unresolved,
        resolved,
        run: Box::new(move |_| {
            Ok(links
                .iter()
                .map(|(path, target)| {
                    (
                        path.clone(),
                        Entry::Link {
                            target: target.clone(),
                        },
                    )
                })
                .collect())
        }),
    };
    Ok((action, missing))
}

/// The targets a unit's `[Install]` section adds it to.
fn installed_into(unit: &str) -> Vec<String> {
    let mut section = "";
    let mut targets = Vec::new();
    for line in unit.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line;
        } else if section == "[Install]"
            && let Some(names) = line.strip_prefix("WantedBy=")
        {
            targets.extend(names.split_whitespace().map(str::to_owned));
        }
    }
    targets
}

/// A model's actions, the packages it names that no recipe builds, and the units it enables that
/// no package it has provides.
struct Lowered<'a> {
    actions: Vec<Action<'a>>,
    skipped: Vec<String>,
    unfound: Vec<String>,
}

/// nixpkgs names whose recipe goes by another name.
const RENAMED: &[(&str, &str)] = &[
    ("bashInteractive", "bash"),
    ("gnugrep", "grep"),
    ("gnused", "sed"),
];

/// The recipe that builds `name`, a package or `package.output` from the model.
fn recipe_for(name: &str, known: impl Fn(&str) -> bool) -> Option<&str> {
    let base = name.split_once('.').map_or(name, |(base, _)| base);
    let id = RENAMED
        .iter()
        .find(|(from, _)| *from == base)
        .map_or(base, |(_, to)| *to);
    known(id).then_some(id)
}

/// The recipes for the model's packages, and the names no recipe builds, each once.
fn model_packages(
    model: &Value,
    known: impl Fn(&str) -> bool,
) -> Result<(Vec<&str>, Vec<String>), String> {
    let (mut named, mut skipped) = (Vec::new(), Vec::new());
    let listed = ["packages", "service_packages"]
        .into_iter()
        .flat_map(|key| model[key].as_array().into_iter().flatten());
    for value in listed {
        let name = str_field(value, "package")?;
        match recipe_for(name, &known) {
            Some(id) if !named.contains(&id) => named.push(id),
            Some(_) => {}
            None if !skipped.iter().any(|seen| seen == name) => skipped.push(name.to_owned()),
            None => {}
        }
    }
    Ok((named, skipped))
}

/// The programs the initrd runs, all of each package's: its init and the tools for the disks.
const INITRD_PACKAGES: [&str; 9] = [
    "systemd",
    "bash",
    "coreutils",
    "sed",
    "util-linux",
    "kmod",
    "device-mapper",
    "cryptsetup",
    "btrfs-progs",
];

/// device-mapper's udev rules, which name unlocked containers so they can be mounted.
const DM_RULES: [&str; 3] = ["10-dm.rules", "13-dm-disk.rules", "95-dm-notify.rules"];

/// How the initrd is put together, in its action's key: what it's made of is decided in code here,
/// so a change to that changes this, and the key with it.
const INITRD_ASSEMBLY: &str = "systemd; packages without build files; the libraries they load; \
     kmod by its names; udev rules in /etc";

const INITRD_RELEASE: &str = "NAME=distro\nID=distro\nPRETTY_NAME=\"distro (initrd)\"\n";

/// The initrd's own files, by path, where `@id@` stands for package `id`'s path: systemd's
/// configuration, accounts, how to unlock and mount the disks, and the service that readies the
/// root.
fn initrd_files(disks: &early::Disks, hostname: &str) -> Vec<(String, u32, String)> {
    let unit = "[Unit]\nDescription=Ready the root to switch to\nDefaultDependencies=no\n\
        RequiresMountsFor=/sysroot /sysroot/store\nAfter=initrd-fs.target\n\
        Before=initrd.target\n\n[Service]\nType=oneshot\nRemainAfterExit=yes\n\
        ExecStart=/etc/distro-root\n";
    // A shell on the console when the root can't be reached, so the machine can be fixed. The
    // initrd's root has no password, and only the console reaches it.
    let force_shell = "[Service]\nExecStart=\nExecStart=-/bin/sh\n";
    let groups: String = [
        "tty", "disk", "kmem", "input", "video", "audio", "lp", "dialout", "cdrom", "tape",
        "floppy", "render", "sgx", "clock", "kvm",
    ]
    .iter()
    .enumerate()
    .map(|(n, group)| format!("{group}:x:{}:\n", n + 1))
    .collect();
    let file = |path: &str, mode: u32, text: String| (path.to_owned(), mode, text);
    let modules: String = disks
        .modules
        .iter()
        .map(|module| format!("{module}\n"))
        .collect();
    let system_conf = "[Manager]\nDefaultEnvironment=PATH=/bin\nManagerEnvironment=PATH=/bin\n";
    let mut files = vec![
        file("etc/initrd-release", 0o644, INITRD_RELEASE.to_owned()),
        file("etc/os-release", 0o644, INITRD_RELEASE.to_owned()),
        file("etc/systemd/system.conf", 0o644, system_conf.to_owned()),
        file(
            "etc/systemd/system/distro-root.service",
            0o644,
            unit.to_owned(),
        ),
        file(
            "etc/systemd/system/emergency.service.d/distro.conf",
            0o644,
            force_shell.to_owned(),
        ),
        file(
            "etc/systemd/system/rescue.service.d/distro.conf",
            0o644,
            force_shell.to_owned(),
        ),
        file("etc/distro-root", 0o755, INITRD_ROOT.to_owned()),
        file("etc/fstab", 0o644, disks.initrd_fstab.clone()),
        file("etc/crypttab", 0o644, disks.crypttab.clone()),
        file(
            "etc/passwd",
            0o644,
            "root:x:0:0:root:/root:/bin/sh\n".to_owned(),
        ),
        file("etc/group", 0o644, format!("root:x:0:\n{groups}")),
        file("etc/shadow", 0o600, "root:*:::::::\n".to_owned()),
        file("etc/modules-load.d/distro.conf", 0o644, modules),
    ];
    if !hostname.is_empty() {
        files.push(file("etc/hostname", 0o644, format!("{hostname}\n")));
    }
    files
}

/// The kernel and an initrd that reaches the root: systemd, which unlocks the disks the model
/// describes, mounts the root under /sysroot and switches to the `init=` on the kernel's command
/// line. Without disks, the root is the one `root=` names.
fn boot_files<'a>(
    lab: &'a Lab,
    model: &Value,
    packages: &'a Packages,
) -> Result<Action<'a>, String> {
    let disks = early::disks(&model["base"]["storage"]);
    let hostname = model["base"]["system"]["hostname"]
        .as_str()
        .unwrap_or_default();
    let files = initrd_files(&disks, hostname);
    let ids = [INITRD_PACKAGES.as_slice(), &["linux"]].concat();
    let (unresolved, resolved) = action_records(&ids, packages, |deps| {
        json!({"kind": "boot", "assembly": INITRD_ASSEMBLY, "files": files, "rules": DM_RULES,
               "outputs": ["initrd", "kernel"], "deps": deps})
    })?;
    Ok(Action {
        origin: "base.boot".to_owned(),
        unresolved,
        resolved,
        run: Box::new(move |_| {
            let linux = &package(packages, "linux")?.path;
            let systemd_path = &package(packages, "systemd")?.path;
            let systemd = systemd_path.inside();
            let mut nodes = initrd::Nodes::new();
            for dir in ["proc", "sys", "dev", "run", "sysroot", "tmp", "var", "root"] {
                initrd::insert(&mut nodes, dir, initrd::Node::Dir);
            }
            let console = initrd::Node::Char { major: 5, minor: 1 };
            initrd::insert(&mut nodes, "dev/console", console);
            let link = |target: String| initrd::Node::Link(target);
            initrd::insert(
                &mut nodes,
                "init",
                link(format!("{systemd}/lib/systemd/systemd")),
            );
            initrd::insert(
                &mut nodes,
                "etc/systemd/system/default.target",
                link(format!("{systemd}/lib/systemd/system/initrd.target")),
            );
            initrd::insert(
                &mut nodes,
                "etc/systemd/system/initrd.target.requires/distro-root.service",
                link("../distro-root.service".to_owned()),
            );
            for (path, mode, template) in &files {
                let bytes = fill(template, &ids, packages)?.into_bytes();
                initrd::insert(&mut nodes, path, initrd::Node::File { mode: *mode, bytes });
            }
            let device_mapper = package(packages, "device-mapper")?.path.inside();
            for rule in DM_RULES {
                let target = format!("{device_mapper}/lib/udev/rules.d/{rule}");
                initrd::insert(
                    &mut nodes,
                    &format!("etc/udev/rules.d/{rule}"),
                    link(target),
                );
            }
            // udev reads its rules from /etc, and systemd's own name the disks, by label among
            // others.
            let rules = lab.on_host(systemd_path).join("lib/udev/rules.d");
            for entry in fs::read_dir(&rules).into_iter().flatten().flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let target = format!("{systemd}/lib/udev/rules.d/{name}");
                initrd::insert(
                    &mut nodes,
                    &format!("etc/udev/rules.d/{name}"),
                    link(target),
                );
            }
            // Every program of the packages on the path, the way a system's `sw/bin` has them.
            let mut roots = Vec::new();
            for id in INITRD_PACKAGES {
                let path = &package(packages, id)?.path;
                roots.push(path.hash());
                let bin = lab.on_host(path).join("bin");
                for entry in fs::read_dir(&bin).into_iter().flatten().flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let target = format!("{}/bin/{name}", path.inside());
                    nodes
                        .entry(format!("bin/{name}"))
                        .or_insert_with(|| link(target));
                }
            }
            initrd::insert(&mut nodes, "bin/sh", link("bash".to_owned()));
            // kmod is one program that behaves by the name it's called, and the kernel calls
            // modprobe.
            for name in ["modprobe", "insmod", "rmmod", "lsmod", "modinfo", "depmod"] {
                initrd::insert(&mut nodes, &format!("bin/{name}"), link("kmod".to_owned()));
            }
            initrd::insert(&mut nodes, "sbin", link("bin".to_owned()));
            initrd::add_programs(&mut nodes, &lab.work.join("store"), &roots)?;
            let modules = lab.on_host(linux).join("lib/modules");
            if modules.exists() {
                let to = format!("store/{}/lib/modules", linux.hash());
                initrd::add_tree(&mut nodes, &modules, &to)?;
                let target = format!("{}/lib/modules", linux.inside());
                initrd::insert(&mut nodes, "lib/modules", link(target));
            }
            let compressed = zstd::bulk::compress(&initrd::archive(&nodes), 3)
                .map_err(|e| format!("compressing the initrd: {e}"))?;
            let path = StorePath::from_hash(&digest(&compressed));
            let directory = lab.on_host(&path);
            fs::create_dir_all(&directory).map_err(with_path(&directory))?;
            let file = directory.join("initrd");
            fs::write(&file, compressed).map_err(with_path(&file))?;
            let link = |target: String| Entry::Link { target };
            Ok(BTreeMap::from([
                (
                    "initrd".to_owned(),
                    link(format!("{}/initrd", path.inside())),
                ),
                (
                    "kernel".to_owned(),
                    link(format!("{}/bzImage", linux.inside())),
                ),
            ]))
        }),
    })
}

/// What the UEFI boot test adds to its systems: a service that reports how each boot went, on the
/// serial port, switches live on the first one, and powers off.
fn report_actions(packages: &Packages) -> Result<Vec<Action<'_>>, String> {
    let origin = "e2e";
    Ok(vec![
        text(
            origin,
            "etc/systemd/system/report.service",
            0o644,
            REPORT_UNIT.into(),
            &[],
            packages,
        )?,
        text(origin, "report", 0o755, REPORT.into(), &["bash"], packages)?,
        links_to(
            origin,
            &[(
                "etc/systemd/system/multi-user.target.wants/report.service",
                "../report.service",
            )],
            packages,
        )?,
    ])
}

/// Links every system has, where `@systemd@` stands for systemd's path. They're part of the
/// action's key, so changing one changes the action.
const FIXED_LINKS: [(&str, &str); 5] = [
    // distro starts systemd from here at boot.
    ("systemd", "@systemd@"),
    (
        "etc/systemd/system/sockets.target.wants/dbus.socket",
        "../dbus.socket",
    ),
    (
        "etc/systemd/system/default.target",
        "@systemd@/lib/systemd/system/multi-user.target",
    ),
    // It stats /usr to decide whether /etc is up to date, and there is no /usr.
    (
        "etc/systemd/system/systemd-update-done.service",
        "/dev/null",
    ),
    // A login prompt on the screen.
    (
        "etc/systemd/system/getty.target.wants/getty@tty1.service",
        "@systemd@/lib/systemd/system/getty@.service",
    ),
];

fn fixed_links(packages: &Packages) -> Result<Action<'_>, String> {
    links_to("base", &FIXED_LINKS, packages)
}

/// Links, each from a path in the generation to a target where `@systemd@` stands for systemd's
/// path. They're part of the action's key.
fn links_to<'a>(
    origin: &str,
    links: &'static [(&'static str, &'static str)],
    packages: &'a Packages,
) -> Result<Action<'a>, String> {
    let (unresolved, resolved) = action_records(
        &["systemd"],
        packages,
        |deps| json!({"kind": "fixed-links", "links": links, "deps": deps}),
    )?;
    Ok(Action {
        origin: origin.to_owned(),
        unresolved,
        resolved,
        run: Box::new(move |_| {
            links
                .iter()
                .map(|(path, target)| {
                    let target = fill(target, &["systemd"], packages)?;
                    Ok(((*path).to_owned(), Entry::Link { target }))
                })
                .collect()
        }),
    })
}

/// What realising a graph did.
#[derive(Debug)]
pub struct Outcome {
    pub generation: String,
    pub actions: usize,
    pub ran: usize,
    pub materialized: bool,
    pub files: usize,
    /// For each path, the action and the part of the model that made it.
    pub made: BTreeMap<String, Value>,
}

fn materialize(
    lab: &Lab,
    objects: &Objects,
    name: &str,
    entries: &Entries,
) -> Result<bool, String> {
    let destination = lab.on_host(&StorePath::from_hash(name));
    if destination.exists() {
        return Ok(false);
    }
    let temporary = destination.with_extension(format!("tmp{}", std::process::id()));
    for (path, entry) in entries {
        let file = temporary.join(path);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).map_err(with_path(parent))?;
        }
        match entry {
            Entry::File { mode, object } => {
                let from = objects.0.join(object);
                fs::copy(&from, &file).map_err(with_path(&from))?;
                fs::set_permissions(&file, fs::Permissions::from_mode(*mode))
                    .map_err(with_path(&file))?;
            }
            Entry::Link { target } => symlink(target, &file).map_err(with_path(&file))?,
        }
    }
    fs::rename(&temporary, &destination).map_err(with_path(&destination))?;
    Ok(true)
}

/// Runs what isn't known yet, then puts the generation together. All in one transaction.
fn realise(lab: &Lab, db: &mut Db, actions: Vec<Action>) -> Result<Outcome, String> {
    let objects = objects_of(lab);
    fs::create_dir_all(&objects.0).map_err(with_path(&objects.0))?;
    let tx = db.0.transaction().map_err(sql)?;
    let total = actions.len() + 1;
    let (mut ran, mut merged) = (0, Entries::new());
    let mut parts: Vec<(String, String)> = Vec::new();
    let mut made: BTreeMap<String, Value> = BTreeMap::new();
    let (mut unresolved_parts, mut resolved_parts) = (Map::new(), Map::new());
    for action in actions {
        let (unresolved, resolved) = (key_of(&action.unresolved), key_of(&action.resolved));
        put_action(&tx, &unresolved, &action.unresolved)?;
        put_action(&tx, &resolved, &action.resolved)?;
        let text = match output_of(&tx, &resolved)? {
            Some(text) => text,
            None => {
                ran += 1;
                let text =
                    serde_json::to_string(&(action.run)(&objects)?).map_err(|e| e.to_string())?;
                put_output(&tx, &resolved, &text)?;
                put_output(&tx, &unresolved, &text)?;
                text
            }
        };
        unresolved_parts.insert(unresolved, json!(true));
        resolved_parts.insert(digest(text.as_bytes()), json!(true));
        let entries: Entries = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        for (path, entry) in entries {
            let line = serde_json::to_string(&entry).map_err(|e| e.to_string())?;
            parts.push((path.clone(), line));
            made.entry(path.clone())
                .or_insert_with(|| json!({"action": resolved, "origin": action.origin}));
            match merged.get(&path) {
                Some(existing) if *existing != entry => {
                    return Err(format!(
                        "`{path}` comes from two actions with different contents"
                    ));
                }
                _ => {
                    merged.insert(path, entry);
                }
            }
        }
    }
    let name = digest(
        serde_json::to_string(&merged)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    );
    let unresolved = json!({"kind": "generation", "parts": unresolved_parts});
    let resolved = json!({"kind": "generation", "parts": resolved_parts});
    let unresolved_key = key_of(&unresolved);
    let resolved_key = key_of(&resolved);
    put_action(&tx, &unresolved_key, &unresolved)?;
    put_action(&tx, &resolved_key, &resolved)?;
    put_output(&tx, &unresolved_key, &name)?;
    put_output(&tx, &resolved_key, &name)?;
    let files = merged.len();
    let materialized = materialize(lab, &objects, &name, &merged)?;
    if materialized {
        ran += 1;
    }
    for (path, entry) in parts {
        tx.prepare_cached("INSERT OR IGNORE INTO parts VALUES (?1, ?2, ?3)")
            .and_then(|mut s| s.execute(params![name, path, entry]))
            .map_err(sql)?;
    }
    tx.commit().map_err(sql)?;
    Ok(Outcome {
        generation: name,
        actions: total,
        ran,
        materialized,
        files,
        made,
    })
}

/// One row of the history. Provenance hangs off this, never off a generation or an action: both
/// are named by content, so two applies can reach them with different answers.
struct Apply {
    kind: String,
    sources: String,
    lock: String,
    explanation: String,
    model: String,
    generation: String,
}

/// Who runs an apply: the process's uid and its name in `passwd`, the person `sudo` ran it for,
/// and the name an agent is given in `DISTRO_AGENT`. `$USER` is left out, since anything can set
/// it, and `SUDO_USER` is only believed from root, the only caller `sudo` leaves behind.
fn caller(uid: u32, passwd: &str, var: impl Fn(&str) -> Option<String>) -> Value {
    let uid_field = uid.to_string();
    let name = passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        (fields.nth(1)? == uid_field).then_some(name)
    });
    let sudo_user = if uid == 0 { var("SUDO_USER") } else { None };
    json!({"uid": uid, "name": name, "sudo_user": sudo_user, "agent": var("DISTRO_AGENT")})
}

/// This process's real uid.
fn uid() -> Result<u32, String> {
    let status = fs::read_to_string("/proc/self/status").map_err(|e| e.to_string())?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|ids| ids.split_whitespace().next()?.parse().ok())
        .ok_or_else(|| "no uid in /proc/self/status".to_owned())
}

fn insert_apply(db: &Db, apply: &Apply) -> Result<i64, String> {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
    let who = caller(uid()?, &passwd, |name| std::env::var(name).ok()).to_string();
    db.0.execute(
        "INSERT INTO applies(time, who, kind, sources, lock, explanation, model, generation,
             previous)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, (SELECT MAX(seq) FROM applies))",
        params![
            time,
            who,
            apply.kind,
            apply.sources,
            apply.lock,
            apply.explanation,
            apply.model,
            apply.generation
        ],
    )
    .map_err(sql)?;
    Ok(db.0.last_insert_rowid())
}

/// Apply number `seq`, or an error when there was none.
fn apply_at(db: &Db, seq: i64) -> Result<Apply, String> {
    db.0.query_row(
        "SELECT kind, sources, lock, explanation, model, generation FROM applies WHERE seq = ?1",
        [seq],
        |row| {
            Ok(Apply {
                kind: row.get(0)?,
                sources: row.get(1)?,
                lock: row.get(2)?,
                explanation: row.get(3)?,
                model: row.get(4)?,
                generation: row.get(5)?,
            })
        },
    )
    .optional()
    .map_err(sql)?
    .ok_or_else(|| format!("there was no apply {seq}"))
}

fn objects_of(lab: &Lab) -> Objects {
    Objects(lab.work.join("e2e/objects"))
}

/// What applies keep: the configuration's files, the lock and the explanation. Apart from the
/// objects, so publishing a generation's objects never publishes the configuration behind it.
fn provenance_of(lab: &Lab) -> Objects {
    Objects(lab.work.join("e2e/provenance"))
}

/// Records an apply of `model`. `inputs` is what `distro dev apply-record` prints.
fn record_apply(
    lab: &Lab,
    db: &Db,
    model: &Value,
    outcome: &Outcome,
    inputs: &Value,
) -> Result<i64, String> {
    let provenance = provenance_of(lab);
    fs::create_dir_all(&provenance.0).map_err(with_path(&provenance.0))?;
    let put = |value: &Value| provenance.put(value.to_string().as_bytes());
    let explanation = json!({"paths": outcome.made, "evaluation": inputs["explanation"]});
    insert_apply(
        db,
        &Apply {
            kind: "apply".to_owned(),
            sources: put(&inputs["sources"])?,
            lock: put(&inputs["lock"])?,
            explanation: put(&explanation)?,
            model: key_of(model),
            generation: outcome.generation.clone(),
        },
    )
}

/// Records going back to apply `seq` as a new apply with its inputs and generation. Returns the
/// new apply's sequence number.
pub fn rollback(lab: &Lab, seq: i64) -> Result<i64, String> {
    let db = Db::open(&db_path(lab))?;
    let apply = apply_at(&db, seq)?;
    insert_apply(
        &db,
        &Apply {
            kind: "rollback".to_owned(),
            ..apply
        },
    )
}

fn db_path(lab: &Lab) -> PathBuf {
    lab.work.join("e2e/graph.sqlite")
}

fn known_packages(built: &Built) -> Packages {
    built
        .paths
        .iter()
        .map(|(id, path)| {
            (
                id.clone(),
                PackageRef {
                    path: path.clone(),
                    unresolved: built.unresolved_or_path(id),
                },
            )
        })
        .collect()
}

fn record_packages(db: &mut Db, built: &Built) -> Result<(), String> {
    let tx = db.0.transaction().map_err(sql)?;
    for (id, record) in &built.records {
        let (unresolved, resolved) = (&record.unresolved, &record.resolved);
        put_action(&tx, unresolved, &record.text)?;
        put_action(&tx, resolved, &record.text)?;
        if let Some(path) = built.paths.get(id) {
            put_output(&tx, unresolved, path.hash())?;
            put_output(&tx, resolved, path.hash())?;
        }
    }
    tx.commit().map_err(sql)
}

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(path).map_err(with_path(path))?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// The recipe files systems are built from, in `recipes/`.
pub const SYSTEM_RECIPES: [&str; 7] = [
    "pilot.toml",
    "full.toml",
    "storage.toml",
    "boot.toml",
    "e2e.toml",
    "rust.toml",
    "distro.toml",
];

/// A system built from a model.
pub struct System {
    pub generation: String,
    /// The generation on the host.
    pub path: PathBuf,
    /// Packages the model names that have no recipe, so the system goes without them.
    pub skipped: Vec<String>,
    /// Units the model enables that no package the system has provides.
    pub unfound: Vec<String>,
    /// The apply recorded, when there were inputs to record.
    pub apply: Option<i64>,
    pub outcome: Outcome,
}

/// Lowers `model` with `packages` and realises it, and records an apply with `inputs`, what
/// `distro dev apply-record` prints, when given.
fn system_from(
    lab: &Lab,
    db: &mut Db,
    packages: &Packages,
    model: &Value,
    kept: &Kept,
    inputs: Option<&Value>,
    report: bool,
) -> Result<System, String> {
    let Lowered {
        mut actions,
        skipped,
        unfound,
    } = lower(lab, model, kept, packages)?;
    if report {
        actions.extend(report_actions(packages)?);
    }
    let outcome = realise(lab, db, actions)?;
    let apply = match inputs {
        Some(inputs) => Some(record_apply(lab, db, model, &outcome, inputs)?),
        None => None,
    };
    Ok(System {
        path: lab.on_host(&StorePath::from_hash(&outcome.generation)),
        generation: outcome.generation.clone(),
        skipped,
        unfound,
        apply,
        outcome,
    })
}

/// Builds the system `model` describes from the recipes in `repository`, which holds `recipes/`,
/// the pins and the `work` directory with the store, building packages it doesn't have yet.
pub fn system(
    repository: &Path,
    model: &Value,
    kept: &Kept,
    inputs: Option<&Value>,
) -> Result<System, String> {
    let lab = Lab::new(repository.join("work"));
    let (built, ..) = pilot::build_recipes(&lab, repository, &SYSTEM_RECIPES, Arm::F)?;
    let mut db = Db::open(&db_path(&lab))?;
    record_packages(&mut db, &built)?;
    system_from(
        &lab,
        &mut db,
        &known_packages(&built),
        model,
        kept,
        inputs,
        false,
    )
}

/// Lowers `model` and realises it. Prints the generation and how long each part took.
pub fn run(
    lab: &Lab,
    repository: &Path,
    recipes: &[&str],
    arm: Arm,
    model_path: &Path,
    inputs_path: Option<&Path>,
) -> Result<(), String> {
    let start = Instant::now();
    let model = read_json(model_path)?;
    let inputs = inputs_path.map(read_json).transpose()?;
    let read = elapsed_ms(start);

    let start = Instant::now();
    let (built, ..) = pilot::build_recipes(lab, repository, recipes, arm)?;
    let packages = known_packages(&built);
    let resolve = elapsed_ms(start);

    let start = Instant::now();
    let mut db = Db::open(&db_path(lab))?;
    record_packages(&mut db, &built)?;
    let records = elapsed_ms(start);

    let start = Instant::now();
    let kept = Kept::plain(&model);
    let system = system_from(
        lab,
        &mut db,
        &packages,
        &model,
        &kept,
        inputs.as_ref(),
        true,
    )?;
    let realised = elapsed_ms(start);
    let skipped: Vec<&str> = system
        .skipped
        .iter()
        .chain(&system.unfound)
        .map(String::as_str)
        .collect();
    if !skipped.is_empty() {
        println!("skipped {}", skipped.join(" "));
    }
    println!("generation {}", system.generation);
    if let Some(seq) = system.apply {
        println!("apply {seq}");
    }
    let outcome = &system.outcome;
    println!(
        "actions {} ran {} files {} new {}",
        outcome.actions, outcome.ran, outcome.files, outcome.materialized
    );
    println!(
        "ms read {read:.1} packages {resolve:.1} records {records:.1} lower and realise \
         {realised:.1}"
    );
    Ok(())
}

fn parts_of(db: &Db, generation: &str) -> Result<BTreeMap<String, String>, String> {
    let mut statement =
        db.0.prepare("SELECT path, entry FROM parts WHERE generation = ?1")
            .map_err(sql)?;
    let rows = statement
        .query_map([generation], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(sql)?;
    rows.collect::<Result<_, _>>().map_err(sql)
}

/// The generation apply `seq` reached, and the explanation it recorded.
fn explained(lab: &Lab, db: &Db, seq: i64) -> Result<(String, Value), String> {
    let apply = apply_at(db, seq)?;
    let explanation = read_json(&provenance_of(lab).0.join(&apply.explanation))?;
    Ok((apply.generation, explanation))
}

/// Where the values behind a path came from, as `web.port set at a.toml:1:1`: each option a
/// module that wrote `origin` read, whose value or source differs from the `other` apply's.
/// Modules record what they read as option prefixes, so `web` covers `web.port`.
fn set_at(explanation: &Value, other: &Value, origin: &str) -> BTreeSet<String> {
    let output = origin
        .strip_prefix("files.")
        .or_else(|| origin.strip_prefix("units."))
        .unwrap_or(origin);
    let evaluation = &explanation["evaluation"];
    let other_options = &other["evaluation"]["options"];
    let reads: Vec<&str> = evaluation["modules"]
        .as_object()
        .into_iter()
        .flat_map(Map::values)
        .filter(|module| {
            module["outputs"]
                .as_array()
                .is_some_and(|o| o.contains(&output.into()))
        })
        .flat_map(|module| module["reads"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .collect();
    let read = |option: &str| {
        reads.iter().any(|prefix| {
            option
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
        })
    };
    let mut found = BTreeSet::new();
    for (option, resolved) in evaluation["options"].as_object().into_iter().flatten() {
        if !read(option) || other_options.get(option) == Some(resolved) {
            continue;
        }
        for set in resolved["set_by"].as_array().into_iter().flatten() {
            if let Some(at) = set["origin"].as_str() {
                found.insert(format!("{option} {at}"));
            }
        }
    }
    found
}

/// What changes going from one apply to another: each file with the action and the part of the
/// model behind it and where its values were set, as each apply recorded it, and which units
/// restart. Applies are named by number, since two can reach one generation for different reasons.
pub fn diff(lab: &Lab, first: i64, second: i64) -> Result<String, String> {
    let db = Db::open(&db_path(lab))?;
    let (first, explained_old) = explained(lab, &db, first)?;
    let (second, explained_new) = explained(lab, &db, second)?;
    let (old, new) = (parts_of(&db, &first)?, parts_of(&db, &second)?);
    let mut report = String::new();
    let mut changed = BTreeSet::new();
    for path in old.keys().chain(new.keys()).collect::<BTreeSet<_>>() {
        let (before, after) = (old.get(path), new.get(path));
        if before == after {
            continue;
        }
        let what = match (before, after) {
            (None, _) => "added",
            (_, None) => "removed",
            _ => "changed",
        };
        let (explanation, other) = if after.is_some() {
            (&explained_new, &explained_old)
        } else {
            (&explained_old, &explained_new)
        };
        let made = &explanation["paths"][path.as_str()];
        match made["origin"].as_str() {
            Some(origin) => {
                report.push_str(&format!(
                    "{what} {path} action {} from {origin}",
                    made["action"].as_str().unwrap_or("?"),
                ));
                for set in set_at(explanation, other, origin) {
                    report.push_str(&format!(", {set}"));
                }
                report.push('\n');
            }
            None => report.push_str(&format!("{what} {path} unaccounted for\n")),
        }
        changed.insert(format!("/{path}"));
    }
    let kept = read_json(
        &lab.on_host(&StorePath::from_hash(&second))
            .join("etc/distro/model.json"),
    )?;
    let units: BTreeSet<&str> = kept["units"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, unit)| {
            unit["restart_on"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .any(|file| changed.contains(file))
        })
        .map(|(name, _)| name.as_str())
        .collect();
    let units: Vec<&str> = units.into_iter().collect();
    report.push_str(&format!("restarts {}\n", units.join(" ")));
    Ok(report)
}

/// The packages a graph needs, for an image: `path <hash>` for each in their closure, and
/// `root <id> <hash>` for the ones a boot script refers to by name.
pub fn closure(
    lab: &Lab,
    repository: &Path,
    recipes: &[&str],
    arm: Arm,
    model_path: &Path,
) -> Result<String, String> {
    let model = read_json(model_path)?;
    let (built, ..) = pilot::build_recipes(lab, repository, recipes, arm)?;
    let mut ids: BTreeSet<String> = BASE_PACKAGES.iter().map(|id| (*id).to_owned()).collect();
    ids.insert("linux".to_owned());
    let (named, _) = model_packages(&model, |id| built.paths.contains_key(id))?;
    ids.extend(named.into_iter().map(str::to_owned));
    let (mut lines, mut all) = (String::new(), BTreeSet::new());
    for id in &ids {
        let path = built
            .paths
            .get(id)
            .ok_or_else(|| format!("no package called `{id}`"))?;
        lines.push_str(&format!("root {id} {}\n", path.hash()));
        all.extend(chain::runtime_closure(lab, path)?);
    }
    for path in all {
        lines.push_str(&format!("path {}\n", path.hash()));
    }
    Ok(lines)
}

/// How many packages of `recipes` have an unresolved key that was never built: the most that
/// could need building, known before anything is.
pub fn bound(lab: &Lab, repository: &Path, recipes: &[&str], arm: Arm) -> Result<String, String> {
    let keys = chain::unresolved_keys(repository, recipes, arm)?;
    let mut db = Db::open(&db_path(lab))?;
    let tx = db.0.transaction().map_err(sql)?;
    let mut missing = Vec::new();
    for (id, key) in &keys {
        if output_of(&tx, key)?.is_none() {
            missing.push(id.as_str());
        }
    }
    Ok(format!(
        "{} of {} packages may need building: {}\n",
        missing.len(),
        keys.len(),
        missing.join(" ")
    ))
}

/// Has `processes` copies of this program write `records` each at once, and reports how it went.
pub fn write_test(processes: usize, records: usize, path: &Path) -> Result<String, String> {
    let start = Instant::now();
    let program = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut children = Vec::new();
    for writer in 0..processes {
        children.push(
            Command::new(&program)
                .arg("graph-writer")
                .arg(path)
                .arg(writer.to_string())
                .arg(records.to_string())
                .spawn()
                .map_err(|e| e.to_string())?,
        );
    }
    let mut failed = 0;
    for mut child in children {
        if !child.wait().map_err(|e| e.to_string())?.success() {
            failed += 1;
        }
    }
    Ok(format!(
        "{processes} writers, {records} records each, in batches: {failed} failed, {:.0} ms\n",
        elapsed_ms(start)
    ))
}

/// One writer of `write_test`: ten transactions, so that writers really interleave.
pub fn write_records(path: &Path, writer: usize, records: usize) -> Result<(), String> {
    let mut db = Db::open(path)?;
    for batch in 0..10 {
        let tx = db.0.transaction().map_err(sql)?;
        for index in 0..records / 10 {
            let record = json!({"writer": writer, "batch": batch, "index": index});
            put_action(&tx, &key_of(&record), &record)?;
        }
        tx.commit().map_err(sql)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(lab: &Lab, name: &str, programs: &[&str]) -> PackageRef {
        let path = StorePath::derive("test", &[name.as_bytes()]);
        let bin = lab.on_host(&path).join("bin");
        fs::create_dir_all(&bin).unwrap();
        for program in programs {
            fs::write(bin.join(program), "").unwrap();
        }
        PackageRef {
            path,
            unresolved: format!("unresolved-{name}"),
        }
    }

    fn setup() -> (tempfile::TempDir, Lab, Packages) {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path().to_str().unwrap());
        fs::create_dir_all(dir.path().join("store")).unwrap();
        let mut packages = Packages::new();
        for id in BASE_PACKAGES
            .iter()
            .chain(INITRD_PACKAGES.iter())
            .chain(["web", "linux"].iter())
        {
            let programs: &[&str] = match *id {
                "web" => &["web"],
                "distro" => &["distro"],
                _ => &[],
            };
            packages.insert((*id).to_owned(), fake(&lab, id, programs));
        }
        (dir, lab, packages)
    }

    fn model(port: u32, comment: bool) -> Value {
        let mut options = json!({"web.port": port});
        if comment {
            options["web.note"] = json!("not read by anything");
        }
        json!({
            "options": options,
            "packages": ["web"],
            "base": {"system": {"hostname": "m"}, "users": {}},
            "files": {
                "/etc/web.conf": {"mode": 420, "text": format!("port {port}\n")},
                "/srv/index.html": {"mode": 420, "text": "hello\n"},
            },
            "units": {"web.service": {
                "restart_on": ["/etc/web.conf"],
                "sections": [
                    {"name": "Service", "entries": [["ExecStart", "web -c /etc/web.conf"]]},
                    {"name": "Install", "entries": [["WantedBy", "multi-user.target"]]},
                ],
            }},
        })
    }

    fn realised(lab: &Lab, db: &mut Db, packages: &Packages, model: &Value) -> Outcome {
        realise(
            lab,
            db,
            lower(lab, model, &Kept::plain(model), packages)
                .unwrap()
                .actions,
        )
        .unwrap()
    }

    #[test]
    fn a_second_run_runs_nothing_and_gives_the_same_generation() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let first = realised(&lab, &mut db, &packages, &model(80, false));
        let second = realised(&lab, &mut db, &packages, &model(80, false));
        assert!(first.ran > 0);
        assert_eq!(second.ran, 0);
        assert_eq!(first.generation, second.generation);
    }

    #[test]
    fn changing_a_value_reruns_only_what_it_touches() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        realised(&lab, &mut db, &packages, &model(80, false));
        // The file, the model the generation keeps and its hash, and the generation.
        assert_eq!(realised(&lab, &mut db, &packages, &model(81, false)).ran, 4);
    }

    #[test]
    fn an_option_nothing_reads_changes_only_the_model_the_generation_keeps() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let first = realised(&lab, &mut db, &packages, &model(80, false));
        let second = realised(&lab, &mut db, &packages, &model(80, true));
        let changed: Vec<&str> = first
            .made
            .iter()
            .filter(|(path, made)| second.made[path.as_str()]["action"] != made["action"])
            .map(|(path, _)| path.as_str())
            .collect();
        assert_eq!(changed, ["etc/distro/model-hash", "etc/distro/model.json"]);
        assert_eq!(second.ran, 3);
    }

    #[test]
    fn a_package_without_a_recipe_is_skipped_and_named() {
        let (_dir, lab, packages) = setup();
        let mut model = model(80, false);
        model["packages"] = json!(["web", "missing", "polkit.bin"]);
        let lowered = lower(&lab, &model, &Kept::plain(&model), &packages).unwrap();
        assert_eq!(lowered.skipped, ["missing", "polkit.bin"]);
    }

    #[test]
    fn a_system_is_its_generation_what_it_skipped_and_the_apply_it_recorded() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let mut model = model(80, false);
        model["packages"] = json!(["web", "missing"]);

        let built = system_from(
            &lab,
            &mut db,
            &packages,
            &model,
            &Kept::plain(&model),
            None,
            false,
        )
        .unwrap();
        assert_eq!(built.skipped, ["missing"]);
        assert_eq!(built.apply, None);
        assert_eq!(
            built.path,
            lab.on_host(&StorePath::from_hash(&built.generation))
        );
        assert!(built.path.join("etc/web.conf").exists());

        let inputs = inputs("a.toml", 80);
        let again = system_from(
            &lab,
            &mut db,
            &packages,
            &model,
            &Kept::plain(&model),
            Some(&inputs),
            false,
        )
        .unwrap();
        assert_eq!(again.generation, built.generation);
        assert_eq!(again.outcome.ran, 0);
        let seq = again.apply.unwrap();
        assert_eq!(row(&db, seq).3, built.generation);
    }

    #[test]
    fn enabled_units_go_into_the_targets_named_or_those_they_install_into() {
        let (_dir, lab, packages) = setup();
        let units = lab
            .on_host(&packages["systemd"].path)
            .join("lib/systemd/system");
        fs::create_dir_all(&units).unwrap();
        let install = "[Unit]\nWantedBy=nothing.target\n[Install]\nWantedBy=a.target b.target\n";
        fs::write(units.join("x.service"), install).unwrap();
        fs::write(units.join("y.target"), "[Unit]\n").unwrap();
        let mut model = model(80, false);
        model["enabled"] =
            json!({"x.service": [], "y.target": ["multi-user.target"], "z.socket": []});
        let lowered = lower(&lab, &model, &Kept::plain(&model), &packages).unwrap();
        assert!(lowered.skipped.is_empty(), "{:?}", lowered.skipped);
        assert_eq!(lowered.unfound, ["z.socket"]);
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realise(&lab, &mut db, lowered.actions).unwrap();
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let systemd = packages["systemd"].path.inside();
        for (link, unit) in [
            ("a.target.wants/x.service", "x.service"),
            ("b.target.wants/x.service", "x.service"),
            ("multi-user.target.wants/y.target", "y.target"),
        ] {
            let target = fs::read_link(root.join("etc/systemd/system").join(link)).unwrap();
            assert_eq!(
                target,
                Path::new(&format!("{systemd}/lib/systemd/system/{unit}"))
            );
        }
        assert!(
            !root
                .join("etc/systemd/system/nothing.target.wants")
                .exists()
        );
    }

    #[test]
    fn a_user_gets_the_configured_password_hash_and_shell_or_none() {
        let model = json!({"base": {"users": {
            "ann": {"home": "/home/ann", "initial_hashed_password": "$y$abc"},
            "bo": {"home": "/home/bo", "shell": "/bin/sh"},
        }}});
        let (passwd, _, shadow) = account_files(&model).unwrap();
        assert!(passwd.contains("ann:x:1000:100::/home/ann:/run/current-system/sw/bin/bash\n"));
        assert!(
            passwd.contains("bo:x:1001:100::/home/bo:/bin/sh\n"),
            "{passwd}"
        );
        assert!(shadow.contains("ann:$y$abc:1::::::\n"), "{shadow}");
        assert!(shadow.contains("bo:!:1::::::\n"), "{shadow}");
        assert!(
            shadow.lines().all(|line| line.split(':').count() == 9),
            "{shadow}"
        );
    }

    #[test]
    fn users_join_their_extra_groups() {
        let model = json!({"base": {"users": {
            "ann": {"home": "/home/ann", "extra_groups": ["wheel", "video"]},
            "bo": {"home": "/home/bo", "extra_groups": ["wheel"]},
            "cy": {"home": "/home/cy"},
        }}});
        let (_, group, _) = account_files(&model).unwrap();
        assert!(group.contains("wheel:x:10:ann,bo\n"), "{group}");
        assert!(group.contains("video:x:400:ann\n"), "{group}");
        assert!(group.contains("users:x:100:\n"), "{group}");
    }

    #[test]
    fn each_user_gets_a_home_they_own() {
        let model = json!({"base": {"users": {"ann": {"home": "/home/ann"}}}});
        assert_eq!(homes(&model).unwrap(), "d /home/ann 0700 ann users -\n");
    }

    #[test]
    fn a_prefixed_exec_line_keeps_its_prefix_and_finds_the_program() {
        let (_dir, lab, packages) = setup();
        let web = &packages["web"].path;
        let sections = json!([{"name": "Service", "entries": [
            ["ExecStart", ""],
            ["ExecStart", "-web --once"],
        ]}]);
        let rendered = render_unit(&sections, &[(lab.on_host(web), web.inside())]).unwrap();
        let inside = web.inside();
        assert_eq!(
            rendered,
            format!("[Service]\nExecStart=\nExecStart=-{inside}/bin/web --once\n\n")
        );
    }

    #[test]
    fn nixpkgs_names_and_outputs_find_their_recipe() {
        let (_dir, _lab, packages) = setup();
        let known = |id: &str| packages.contains_key(id);
        assert_eq!(recipe_for("bashInteractive", known), Some("bash"));
        assert_eq!(recipe_for("gnugrep", known), Some("grep"));
        assert_eq!(recipe_for("web.bin", known), Some("web"));
        assert_eq!(recipe_for("web", known), Some("web"));
        assert_eq!(recipe_for("polkit", known), None);
    }

    #[test]
    fn a_unit_from_a_package_every_system_has_can_be_enabled() {
        let (_dir, lab, packages) = setup();
        let units = lab
            .on_host(&packages["distro"].path)
            .join("lib/systemd/system");
        fs::create_dir_all(&units).unwrap();
        fs::write(units.join("distro-agent.service"), "[Service]\n").unwrap();
        let mut model = model(80, false);
        model["enabled"] = json!({"distro-agent.service": ["multi-user.target"]});
        let lowered = lower(&lab, &model, &Kept::plain(&model), &packages).unwrap();
        assert!(lowered.unfound.is_empty(), "{:?}", lowered.unfound);
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realise(&lab, &mut db, lowered.actions).unwrap();
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let wants = root.join("etc/systemd/system/multi-user.target.wants/distro-agent.service");
        assert!(fs::symlink_metadata(wants).is_ok());
        // systemd finds a wanted unit by its name, on its own search path.
        let unit = fs::read_link(root.join("etc/systemd/system/distro-agent.service")).unwrap();
        let distro = packages["distro"].path.inside();
        assert_eq!(
            unit,
            Path::new(&format!("{distro}/lib/systemd/system/distro-agent.service"))
        );
    }

    #[test]
    fn the_initrd_unlocks_and_mounts_what_the_model_describes_then_readies_the_root() {
        let disks = early::Disks {
            crypttab: "main0-system /dev/disk/by-partlabel/main0-system none luks\n".into(),
            initrd_fstab: "/dev/mapper/main0-system /sysroot/store btrfs subvol=@store 0 0\n"
                .into(),
            modules: vec!["dm_crypt".into(), "btrfs".into()],
            ..early::Disks::default()
        };
        let files: BTreeMap<String, String> = initrd_files(&disks, "w-01")
            .into_iter()
            .map(|(path, _, text)| (path, text))
            .collect();
        assert_eq!(files["etc/crypttab"], disks.crypttab);
        assert_eq!(files["etc/fstab"], disks.initrd_fstab);
        assert_eq!(files["etc/modules-load.d/distro.conf"], "dm_crypt\nbtrfs\n");
        assert_eq!(files["etc/hostname"], "w-01\n");
        assert!(files["etc/initrd-release"].contains("ID=distro\n"));
        let root = &files["etc/systemd/system/distro-root.service"];
        assert!(root.contains("ExecStart=/etc/distro-root\n"), "{root}");
        assert!(files["etc/distro-root"].contains("os-release"));
    }

    #[test]
    fn a_systems_initrd_changes_when_its_disks_do() {
        let (_dir, lab, packages) = setup();
        let mut model = model(80, false);
        let key = |model: &Value| boot_files(&lab, model, &packages).unwrap().resolved;
        let before = key(&model);
        model["base"]["storage"] = json!({
            "partitions": [{"encrypt": true, "label": "main0-system"}],
            "mounts": [], "unlock": {},
        });
        assert_ne!(key(&model), before);
    }

    #[test]
    fn a_system_calls_itself_distro() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &model(80, false));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let release = fs::read_to_string(root.join("etc/os-release")).unwrap();
        assert_eq!(release, "NAME=distro\nID=distro\n");
    }

    #[test]
    fn a_generation_keeps_the_model_it_was_built_from() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let model = model(80, false);
        let outcome = realised(&lab, &mut db, &packages, &model);
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let kept = Kept::plain(&model);
        let read = |path: &str| fs::read_to_string(root.join(path)).unwrap();
        assert_eq!(read("etc/distro/model.json"), kept.text);
        assert_eq!(read("etc/distro/model-hash"), kept.hash);
    }

    #[test]
    fn a_generation_boots_and_switches_through_distro() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &model(80, false));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let read = |path: &str| fs::read_to_string(root.join(path)).unwrap();
        assert!(read("init").contains("/sw/bin/distro\" activate --mode init \"$self\""));
        let switch = read("bin/switch-to-configuration");
        assert!(switch.contains("/sw/bin/distro\" activate --mode \"$1\" \"$self\""));
        let distro = packages["distro"].path.inside();
        assert_eq!(
            fs::read_link(root.join("sw/bin/distro")).unwrap(),
            Path::new(&format!("{distro}/bin/distro"))
        );
        let systemd = packages["systemd"].path.inside();
        assert_eq!(
            fs::read_link(root.join("systemd")).unwrap(),
            Path::new(&systemd)
        );
        for gone in ["switch", "wrappers", "wrappers.list", "restart-on"] {
            assert!(!root.join(gone).exists(), "{gone}");
        }
    }

    #[test]
    fn the_kernel_and_initrd_are_at_the_top_with_the_kernels_options_beside_them() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let mut model = model(80, false);
        model["base"]["boot"] = json!({"kernel_params": ["quiet", "loglevel=3"]});
        let outcome = realised(&lab, &mut db, &packages, &model);
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let linux = packages["linux"].path.inside();
        assert_eq!(
            fs::read_link(root.join("kernel")).unwrap(),
            Path::new(&format!("{linux}/bzImage"))
        );
        assert!(
            fs::read_link(root.join("initrd"))
                .unwrap()
                .ends_with("initrd")
        );
        let params = fs::read_to_string(root.join("kernel-params")).unwrap();
        assert_eq!(
            params,
            format!("root=LABEL={ROOT_LABEL} rw quiet loglevel=3\n")
        );
    }

    #[test]
    fn activation_lists_the_etc_files_with_a_mode_of_their_own() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let mut model = model(80, false);
        model["files"]["/etc/web.key"] = json!({"mode": 0o600, "text": "key\n"});
        let outcome = realised(&lab, &mut db, &packages, &model);
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let text = fs::read_to_string(root.join("activation.json")).unwrap();
        let activation: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(activation["etc_modes"], json!({"web.key": "0600"}));
        let bash = packages["bash"].path.inside();
        assert_eq!(activation["bash"], json!(format!("{bash}/bin/bash")));
        assert_eq!(activation["homes"], json!([]));
    }

    #[test]
    fn every_file_in_a_generation_says_where_it_came_from() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &model(80, true));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let found: Vec<String> = pilot::files_under(&root)
            .unwrap()
            .iter()
            .map(|path| path.strip_prefix(&root).unwrap().display().to_string())
            .collect();
        assert!(found.len() > 10, "{found:?}");
        let unexplained: Vec<&String> = found
            .iter()
            .filter(|path| {
                let made = outcome.made.get(path.as_str());
                made.and_then(|made| made["origin"].as_str()).is_none()
            })
            .collect();
        assert!(unexplained.is_empty(), "no origin for {unexplained:?}");
    }

    #[test]
    fn units_use_the_program_from_the_package_and_are_enabled() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &model(80, false));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let unit = fs::read_to_string(root.join("etc/systemd/system/web.service")).unwrap();
        let web = packages["web"].path.inside();
        assert!(
            unit.contains(&format!("ExecStart={web}/bin/web -c /etc/web.conf")),
            "{unit}"
        );
        let wants = root.join("etc/systemd/system/multi-user.target.wants/web.service");
        assert_eq!(fs::read_link(wants).unwrap(), Path::new("../web.service"));
    }

    #[test]
    fn a_diff_names_the_changed_file_its_origin_where_it_was_set_and_what_restarts() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let (_, one) = applied(&lab, &mut db, &packages, 80, "a.toml");
        let (_, two) = applied(&lab, &mut db, &packages, 81, "a.toml");
        let report = diff(&lab, one, two).unwrap();
        assert!(report.contains("changed etc/web.conf"), "{report}");
        assert!(report.contains("from files./etc/web.conf"), "{report}");
        assert!(report.contains("web.port set at a.toml:1:1"), "{report}");
        assert!(!report.contains("web.name"), "{report}");
        assert!(report.ends_with("restarts web.service\n"), "{report}");
    }

    #[test]
    fn applies_that_reach_one_generation_each_cite_their_own_file() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let (_, base) = applied(&lab, &mut db, &packages, 81, "base.toml");
        let (first, one) = applied(&lab, &mut db, &packages, 80, "a.toml");
        let (second, two) = applied(&lab, &mut db, &packages, 80, "b.toml");
        assert_eq!(first.generation, second.generation);
        let (to_one, to_two) = (
            diff(&lab, base, one).unwrap(),
            diff(&lab, base, two).unwrap(),
        );
        assert!(
            to_one.contains("set at a.toml") && !to_one.contains("b.toml"),
            "{to_one}"
        );
        assert!(
            to_two.contains("set at b.toml") && !to_two.contains("a.toml"),
            "{to_two}"
        );
    }

    #[test]
    fn a_diff_says_when_an_apply_cant_account_for_a_file() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let (_, one) = applied(&lab, &mut db, &packages, 80, "a.toml");
        let model = model(81, false);
        let mut outcome = realised(&lab, &mut db, &packages, &model);
        outcome.made.clear();
        let two = record_apply(&lab, &db, &model, &outcome, &inputs("a.toml", 81)).unwrap();
        let report = diff(&lab, one, two).unwrap();
        assert!(
            report.contains("changed etc/web.conf unaccounted for"),
            "{report}"
        );
    }

    #[test]
    fn a_diff_of_an_apply_that_never_happened_is_an_error() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let (_, one) = applied(&lab, &mut db, &packages, 80, "a.toml");
        let error = diff(&lab, one, 99).unwrap_err();
        assert!(error.contains("99"), "{error}");
    }

    /// What distro records for a configuration that sets `web.port` to `port` in `file`. Modules
    /// record what they read as option prefixes.
    fn inputs(file: &str, port: u32) -> Value {
        json!({
            "sources": {file: format!("port = {port}\n")},
            "lock": {},
            "explanation": {
                "options": {
                    "web.port": {"value": port.to_string(), "set_by": [
                        {"origin": format!("set at {file}:1:1"), "value": port.to_string()},
                    ]},
                    "web.name": {"value": "\"web\"", "set_by": [
                        {"origin": "default", "value": "\"web\""},
                    ]},
                },
                "modules": {"web": {"reads": ["web"], "outputs": ["/etc/web.conf"]}},
            },
        })
    }

    fn applied(
        lab: &Lab,
        db: &mut Db,
        packages: &Packages,
        port: u32,
        file: &str,
    ) -> (Outcome, i64) {
        let model = model(port, false);
        let outcome = realised(lab, db, packages, &model);
        let seq = record_apply(lab, db, &model, &outcome, &inputs(file, port)).unwrap();
        (outcome, seq)
    }

    fn row(db: &Db, seq: i64) -> (String, String, String, String, Option<i64>) {
        db.0.query_row(
            "SELECT kind, sources, explanation, generation, previous FROM applies WHERE seq = ?1",
            [seq],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap()
    }

    fn object(lab: &Lab, digest: &str) -> String {
        fs::read_to_string(provenance_of(lab).0.join(digest)).unwrap()
    }

    #[test]
    fn moving_a_value_keeps_the_generation_and_each_apply_its_own_answer() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let (first, one) = applied(&lab, &mut db, &packages, 80, "a.toml");
        let (second, two) = applied(&lab, &mut db, &packages, 80, "b.toml");
        assert_eq!(first.generation, second.generation);
        assert_eq!(second.ran, 0);
        let (before, after) = (row(&db, one), row(&db, two));
        assert_eq!(after.4, Some(one));
        assert!(object(&lab, &before.2).contains("set at a.toml"));
        assert!(object(&lab, &after.2).contains("set at b.toml"));
        assert!(object(&lab, &before.1).contains("a.toml"));
    }

    #[test]
    fn the_same_inputs_applied_twice_are_kept_once() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let (_, one) = applied(&lab, &mut db, &packages, 80, "a.toml");
        let (_, two) = applied(&lab, &mut db, &packages, 80, "a.toml");
        assert_ne!(one, two);
        assert_eq!(row(&db, one).2, row(&db, two).2);
    }

    #[test]
    fn a_rollback_is_a_new_apply_with_the_old_inputs() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let (first, one) = applied(&lab, &mut db, &packages, 80, "a.toml");
        let (_, two) = applied(&lab, &mut db, &packages, 81, "b.toml");
        let seq = rollback(&lab, one).unwrap();
        let (original, back) = (row(&db, one), row(&db, seq));
        assert_eq!(back.0, "rollback");
        assert_eq!(back.1, original.1);
        assert_eq!(back.2, original.2);
        assert_eq!(back.3, first.generation);
        assert_eq!(back.4, Some(two));
    }

    #[test]
    fn rolling_back_to_an_apply_that_never_happened_is_an_error() {
        let (_dir, lab, _) = setup();
        let error = rollback(&lab, 7).unwrap_err();
        assert!(error.contains('7'), "{error}");
    }

    fn env<'a>(vars: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<String> + 'a {
        |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/sh\njohn:x:1000:100::/home/john:/bin/sh\n";

    #[test]
    fn an_apply_records_who_by_uid_and_ignores_user() {
        let who = caller(1000, PASSWD, env(&[("USER", "mallory")]));
        assert_eq!(
            who,
            json!({"uid": 1000, "name": "john", "sudo_user": null, "agent": null})
        );
    }

    #[test]
    fn an_agent_applying_through_sudo_is_recorded_with_the_person_behind_it() {
        let vars = [("SUDO_USER", "john"), ("DISTRO_AGENT", "claude")];
        let who = caller(0, PASSWD, env(&vars));
        assert_eq!(
            who,
            json!({"uid": 0, "name": "root", "sudo_user": "john", "agent": "claude"})
        );
    }

    #[test]
    fn sudo_user_is_only_believed_from_root() {
        let who = caller(1000, PASSWD, env(&[("SUDO_USER", "root")]));
        assert_eq!(who["sudo_user"], Value::Null);
        assert_eq!(caller(4242, PASSWD, env(&[]))["name"], Value::Null);
    }

    #[test]
    fn records_and_outputs_carry_no_provenance() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        applied(&lab, &mut db, &packages, 80, "secret-path.toml");
        for query in ["SELECT record FROM actions", "SELECT output FROM outputs"] {
            let mut statement = db.0.prepare(query).unwrap();
            let rows: Vec<String> = statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert!(!rows.is_empty());
            for text in rows {
                assert!(!text.contains("secret-path"), "{text}");
                assert!(!text.contains("files./etc/web.conf"), "{text}");
            }
        }
        // Publishing a generation's objects must not publish the configuration behind it.
        for entry in fs::read_dir(&objects_of(&lab).0).unwrap() {
            let text = fs::read_to_string(entry.unwrap().path()).unwrap();
            assert!(!text.contains("secret-path"), "{text}");
        }
    }

    #[test]
    fn an_action_keeps_its_key_when_it_comes_from_somewhere_else() {
        let (_dir, _lab, packages) = setup();
        let one = text("here", "etc/x", 0o644, "x".into(), &[], &packages).unwrap();
        let two = text("there", "etc/x", 0o644, "x".into(), &[], &packages).unwrap();
        assert_eq!(key_of(&one.resolved), key_of(&two.resolved));
    }
}
