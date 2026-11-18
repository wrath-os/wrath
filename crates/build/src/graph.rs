//! Step 15: a wrath model lowered into actions, each with two keys. An unresolved key names what
//! an action depends on by those actions' keys, and is known before anything runs. A resolved key
//! names what they really produced. Where an action came from is kept beside it, unhashed.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::chain::{self, Built, names_of, recipe_files};
use crate::early;
use crate::initrd;
use crate::pilot;
use crate::sandbox;
use crate::split;
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
                     PRIMARY KEY(generation, path));",
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
    /// What its tool makes from what packages share, which a generation has made.
    pub indexes: Vec<chain::Index>,
    /// Its outputs a system can run, programs and libraries, which closures are found from.
    pub outputs: Vec<StorePath>,
}

pub type Packages = BTreeMap<String, PackageRef>;

struct Objects(PathBuf);

impl Objects {
    /// Keeps `bytes` under their hash, written whole or not at all: a crash midway leaves a
    /// temporary file, never a short object under the hash.
    fn put(&self, bytes: &[u8]) -> Result<String, String> {
        let hash = digest(bytes);
        let path = self.0.join(&hash);
        if !path.exists() {
            let partial = self.0.join(format!("{hash}.tmp{}", std::process::id()));
            let mut file = fs::File::create(&partial).map_err(with_path(&partial))?;
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(with_path(&partial))?;
            fs::rename(&partial, &path).map_err(with_path(&path))?;
        }
        Ok(hash)
    }

    /// Whether every file in `entries` has its object here.
    fn hold(&self, entries: &Entries) -> bool {
        entries.values().all(|entry| match entry {
            Entry::File { object, .. } => self.0.join(object).is_file(),
            Entry::Link { .. } => true,
        })
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
    let with_lowering = |mut record: Value| {
        if let Some(fields) = record.as_object_mut() {
            fields.insert("lowering".to_owned(), json!(lowering()));
        }
        record
    };
    Ok((
        with_lowering(record(Value::Object(unresolved))),
        with_lowering(record(Value::Object(resolved))),
    ))
}

/// The hash of the code that turns a model into actions, in every action's key: what an action
/// makes is decided there as much as by its inputs, so changing that code reruns the actions.
fn lowering() -> &'static str {
    static LOWERING: OnceLock<String> = OnceLock::new();
    LOWERING.get_or_init(|| {
        let code = [
            include_str!("graph.rs"),
            include_str!("initrd.rs"),
            include_str!("early.rs"),
        ];
        digest(code.concat().as_bytes())
    })
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

/// Directories whose entries are linked whole, directories included: each compiled locale is one.
const WHOLE_ENTRIES: &[&str] = &["lib/locale"];

/// Directories merged from every package, file by file at any depth, as `share` is: what programs
/// look for under `XDG_DATA_DIRS`, such as fonts, icons and portals.
const MERGED_TREES: &[&str] = &["share"];

/// What a merged `share` leaves out: translations and manuals, which programs find in their own
/// package, and what only builds use.
const UNMERGED: &[&str] = &[
    "locale",
    "man",
    "info",
    "doc",
    "gtk-doc",
    "aclocal",
    "pkgconfig",
];

/// The files and links under `root`, as paths relative to it, leaving out [`UNMERGED`] at the top.
fn tree_files(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![String::new()];
    while let Some(relative) = pending.pop() {
        let Ok(listing) = fs::read_dir(root.join(&relative)) else {
            continue;
        };
        for entry in listing.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if relative.is_empty() && UNMERGED.contains(&name.as_str()) {
                continue;
            }
            let path = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(path),
                Ok(_) => found.push(path),
                Err(_) => {}
            }
        }
    }
    found.sort();
    found
}

/// Links in `to` for every file in `dir` of `package`, at any depth for [`MERGED_TREES`], and every
/// directory where `dir` is one of [`WHOLE_ENTRIES`], leaving entries already there alone. A
/// merged tree also takes what the package's library output has there: data its libraries
/// refer to, such as fontconfig's presets or libX11's compose tables, is moved there when the
/// package is split, and programs look for it where the system shares it.
fn linked(
    lab: &Lab,
    package: &PackageRef,
    dir: &str,
    to: &str,
    entries: &mut Entries,
) -> Result<(), String> {
    let merged = MERGED_TREES.contains(&dir);
    let mut roots = vec![&package.path];
    if merged {
        roots.extend(
            package
                .outputs
                .iter()
                .filter(|output| **output != package.path),
        );
    }
    for root in roots {
        let source = lab.on_host(root).join(dir);
        let names = if merged {
            tree_files(&source)
        } else {
            let Ok(listing) = fs::read_dir(&source) else {
                continue;
            };
            let mut names: Vec<_> = listing
                .filter_map(Result::ok)
                .filter(|entry| {
                    let path = entry.path();
                    path.is_file() || (WHOLE_ENTRIES.contains(&dir) && path.is_dir())
                })
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        for name in names {
            let target = format!("{}/{dir}/{name}", root.inside());
            let path = format!("{to}/{name}");
            match entries.get(&path) {
                None => {
                    entries.insert(path, Entry::Link { target });
                }
                // Packages merged file by file can't quietly win over each other: the same file
                // twice is fine, two different ones stop the build.
                Some(Entry::Link { target: first })
                    if merged && *first != target && !same_contents(lab, first, &target)? =>
                {
                    return Err(format!(
                        "`{path}` is in two packages with different contents: {first} and \
                         {target}"
                    ));
                }
                Some(_) => {}
            }
        }
    }
    Ok(())
}

/// The packages whose data a system with `named` shares: those, and every package one of whose
/// programs or libraries their run-time closure reaches (decision 0021).
fn sharing<'a>(
    lab: &Lab,
    named: &[&'a str],
    packages: &'a Packages,
) -> Result<Vec<&'a str>, String> {
    // An output neither here nor in a cache, such as a main output a split left empty, which
    // nothing refers to and so nothing pushes, is nothing the system runs.
    let mut outputs = Vec::new();
    for id in named {
        outputs.extend(
            package(packages, id)?
                .outputs
                .iter()
                .filter(|o| there(lab, o)),
        );
    }
    chain::fetch_closures(lab, outputs.iter().copied())?;
    let mut reached = BTreeSet::new();
    for output in outputs {
        if !reached.contains(output) {
            reached.extend(chain::runtime_closure(lab, output)?);
        }
    }
    Ok(packages
        .iter()
        .filter(|(id, package)| {
            named.contains(&id.as_str()) || package.outputs.iter().any(|o| reached.contains(o))
        })
        .map(|(id, _)| id.as_str())
        .collect())
}

/// Whether two links into the store name files with the same contents.
fn same_contents(lab: &Lab, first: &str, second: &str) -> Result<bool, String> {
    let on_host = |target: &str| match target.strip_prefix("/wrath/store/") {
        Some(rest) => lab.work.join("store").join(rest),
        None => PathBuf::from(target),
    };
    let (first, second) = (on_host(first), on_host(second));
    match (fs::read_link(&first), fs::read_link(&second)) {
        (Ok(a), Ok(b)) => Ok(a == b),
        (Err(_), Err(_)) => Ok(fs::read(&first).map_err(with_path(&first))?
            == fs::read(&second).map_err(with_path(&second))?),
        (Ok(_), Err(_)) | (Err(_), Ok(_)) => Ok(false),
    }
}

/// An action that makes links into packages. `layout` lists directories as (from, to).
fn links<'a>(
    origin: &str,
    lab: &'a Lab,
    ids: Vec<&'a str>,
    layout: &[(&str, &str)],
    packages: &'a Packages,
) -> Result<Action<'a>, String> {
    let layout: Vec<(String, String)> = layout
        .iter()
        .map(|(from, to)| ((*from).to_owned(), (*to).to_owned()))
        .collect();
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

/// The files under `from` of each of `ids` that has some, as (path under `from`, file on the
/// host), the first package's where two have one, as merged links take it.
fn index_inputs(
    lab: &Lab,
    ids: &[&str],
    packages: &Packages,
    from: &str,
) -> Vec<(String, PathBuf)> {
    let mut found: BTreeMap<String, PathBuf> = BTreeMap::new();
    for id in ids {
        let Some(package) = packages.get(*id) else {
            continue;
        };
        let root = lab.on_host(&package.path).join(from);
        for name in tree_files(&root) {
            let file = root.join(&name);
            found.entry(name).or_insert(file);
        }
    }
    found.into_iter().collect()
}

/// For each index a package declares, the files its tool makes from what `ids` have under the
/// index's `from`, such as GSettings' compiled schemas (decision 0020). An index whose `from`
/// none of them has makes nothing.
fn index_actions<'a>(
    origin: &str,
    lab: &'a Lab,
    ids: &[&'a str],
    packages: &'a Packages,
) -> Result<Vec<Action<'a>>, String> {
    let mut actions = Vec::new();
    for (tool, package) in packages {
        for index in &package.indexes {
            let from = index.from.as_str();
            if !MERGED_TREES
                .iter()
                .any(|tree| from.starts_with(&format!("{tree}/")))
            {
                return Err(format!(
                    "`{tool}` indexes `{from}`, which isn't in what packages share"
                ));
            }
            let contributors: Vec<&str> = ids
                .iter()
                .copied()
                .filter(|id| !index_inputs(lab, &[id], packages, from).is_empty())
                .collect();
            if contributors.is_empty() {
                continue;
            }
            let mut deps = vec![tool.as_str()];
            deps.extend(contributors.iter().filter(|id| **id != tool));
            let (unresolved, resolved) = action_records(&deps, packages, |deps| {
                json!({"kind": "index", "from": from, "run": index.run, "deps": deps,
                       "order": contributors})
            })?;
            let key = key_of(&resolved);
            actions.push(Action {
                origin: origin.to_owned(),
                unresolved,
                resolved,
                run: Box::new(move |objects| {
                    let inputs = index_inputs(lab, &contributors, packages, from);
                    run_index(lab, objects, &key, &package.path, &index.run, from, &inputs)
                }),
            });
        }
    }
    Ok(actions)
}

/// Runs an index's tool over a copy of its inputs under `from`, and takes what it adds there.
fn run_index(
    lab: &Lab,
    objects: &Objects,
    key: &str,
    tool: &StorePath,
    run: &str,
    from: &str,
    inputs: &[(String, PathBuf)],
) -> Result<Entries, String> {
    let dir = lab.work.join("index").join(key);
    chain::fresh(&dir)?;
    let tree = dir.join("build");
    let indexed = tree.join(from);
    for (name, file) in inputs {
        let to = indexed.join(name);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(with_path(parent))?;
        }
        fs::copy(file, &to).map_err(with_path(file))?;
    }
    let mut words = run.split_whitespace();
    let program = words
        .next()
        .ok_or_else(|| format!("an index of `{from}` runs nothing"))?;
    let log = dir.join("log");
    let outcome = sandbox::run(
        lab,
        &sandbox::Step {
            name: format!("index {from}"),
            argv: std::iter::once(format!("{}/{program}", tool.inside()))
                .chain(words.map(str::to_owned))
                .collect(),
            inputs: chain::runtime_closure(lab, tool)?.into_iter().collect(),
            output: None,
            view: None,
            build_dir: tree.clone(),
            log: log.clone(),
            env: BTreeMap::new(),
            limits: sandbox::Limits::DEFAULT,
            links: Vec::new(),
            at: None,
        },
    )?;
    if outcome.status != sandbox::Status::Passed {
        let text = fs::read_to_string(&log).unwrap_or_default();
        return Err(format!("indexing `{from}` with `{run}` failed:\n{text}"));
    }
    // What the tool made: files that are new, or that it rewrote, such as a compiled file a
    // package shipped too, which then clashes with that package's link and says so.
    let copied: BTreeMap<&String, &PathBuf> =
        inputs.iter().map(|(name, file)| (name, file)).collect();
    let mut entries = Entries::new();
    for name in tree_files(&indexed) {
        let file = indexed.join(&name);
        let bytes = fs::read(&file).map_err(with_path(&file))?;
        if let Some(input) = copied.get(&name)
            && fs::read(input).map_err(with_path(input))? == bytes
        {
            continue;
        }
        let mode = fs::metadata(&file)
            .map_err(with_path(&file))?
            .permissions()
            .mode()
            & 0o777;
        entries.insert(
            format!("{from}/{name}"),
            Entry::File {
                mode,
                object: objects.put(&bytes)?,
            },
        );
    }
    chain::remove(&dir)?;
    Ok(entries)
}

/// What every system has whatever its platform, since the mechanism needs it: wrath, which
/// switches; bash and coreutils, which its scripts run; systemd, which starts everything, and
/// util-linux and kmod, which it mounts with and loads modules with; D-Bus; and grapes, the set it
/// was built from, which wrath on the machine evaluates the configuration with. The rest of a
/// base system is what the platform's modules add.
const MECHANISM_PACKAGES: &[&str] = &[
    "bash",
    "coreutils",
    "dbus",
    "grapes",
    "wrath",
    "kmod",
    "systemd",
    "util-linux",
];

/// What makes a user's home, under `users/NAME` in the generation: links to the programs of
/// their packages in `bin`, and their files, units among them, in `files`, which wrath on the
/// machine links into the home. Packages no recipe builds join `skipped`.
fn home_actions<'a>(
    lab: &'a Lab,
    user: &str,
    home: &'a Value,
    system: &[&'a str],
    skipped: &mut Vec<String>,
    packages: &'a Packages,
) -> Result<Vec<Action<'a>>, String> {
    let mut actions = Vec::new();
    let mut ids = Vec::new();
    for value in home["packages"].as_array().into_iter().flatten() {
        let id = str_field(value, "package")?;
        if packages.contains_key(id) {
            ids.push(id);
        } else if !skipped.iter().any(|seen| seen == id) {
            skipped.push(id.to_owned());
        }
    }
    let root = format!("users/{user}");
    if !ids.is_empty() {
        let bin = format!("{root}/bin");
        let pairs = [("bin", bin.as_str()), ("sbin", bin.as_str())];
        let origin = format!("users.{user}.packages");
        actions.push(links(&origin, lab, ids.clone(), &pairs, packages)?);
    }
    let origin = format!("users.{user}.files");
    for (path, file) in home["files"].as_object().into_iter().flatten() {
        let relative = path
            .strip_prefix("~/")
            .ok_or_else(|| format!("{user}'s file `{path}` isn't in their home"))?;
        let mode = file["mode"].as_u64().unwrap_or(0o644) as u32;
        let content = str_field(&file["text"], "file's text")?.to_owned();
        let at = format!("{root}/files/{relative}");
        actions.push(text(&origin, &at, mode, content, &[], packages)?);
    }
    // A user's units find the user's programs first, then the system's.
    let unit_ids: Vec<&str> = ids.iter().chain(system).copied().collect();
    let units = format!("{root}/files/{USER_UNITS}");
    for (name, unit) in home["units"].as_object().into_iter().flatten() {
        actions.push(unit_action(
            lab,
            name,
            unit,
            unit_ids.clone(),
            packages,
            &units,
        )?);
    }
    Ok(actions)
}

/// Where a user's units go, in their home.
const USER_UNITS: &str = ".config/systemd/user";

/// Where the system's units go.
const SYSTEM_UNITS: &str = "etc/systemd/system";
/// Where each user's service manager finds units the system adds.
const SYSTEM_USER_UNITS: &str = "etc/systemd/user";

/// The units of one kind of service manager: the model field enabling them, where packages keep
/// them, where the system adds them, and the packages whose units that manager finds already.
struct UnitKind {
    enabled: &'static str,
    packaged: &'static str,
    added: &'static str,
    found: &'static [&'static str],
}

const UNIT_KINDS: [UnitKind; 2] = [
    UnitKind {
        enabled: "enabled",
        packaged: "lib/systemd/system",
        added: SYSTEM_UNITS,
        found: &["systemd", "dbus"],
    },
    UnitKind {
        enabled: "enabled_user",
        packaged: "lib/systemd/user",
        added: SYSTEM_USER_UNITS,
        found: &["systemd"],
    },
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
        (
            "lib/systemd/system-generators",
            "etc/systemd/system-generators",
        ),
        ("share/dbus-1/system.d", "etc/dbus-1/system.d"),
        // PKCS#11 modules, such as GNOME Keyring's, which p11-kit loads.
        ("share/p11-kit/modules", "etc/pkcs11/modules"),
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

/// The packages with firmware for the kernel that the model names.
fn firmware_ids(model: &Value) -> impl Iterator<Item = &str> {
    model["base"]["kernel"]["firmware"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

/// The firmware of each of `ids`, linked into one directory, `firmware`, for the kernel to load.
fn firmware_links<'a>(
    lab: &'a Lab,
    ids: Vec<&'a str>,
    packages: &'a Packages,
) -> Result<Action<'a>, String> {
    let (unresolved, resolved) = action_records(
        &ids,
        packages,
        |deps| json!({"kind": "firmware", "deps": deps}),
    )?;
    Ok(Action {
        origin: "hardware.firmware".to_owned(),
        unresolved,
        resolved,
        run: Box::new(move |_| {
            let mut entries = Entries::new();
            for id in &ids {
                let path = &package(packages, id)?.path;
                let dir = lab.on_host(path).join("lib/firmware");
                let mut names: Vec<String> = fs::read_dir(&dir)
                    .map_err(with_path(&dir))?
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                for name in names {
                    let target = format!("{}/lib/firmware/{name}", path.inside());
                    entries
                        .entry(format!("firmware/{name}"))
                        .or_insert(Entry::Link { target });
                }
            }
            Ok(entries)
        }),
    })
}

/// The kernel's options for the system's boot entries, as the base module worked them out.
fn kernel_params(model: &Value) -> Result<String, String> {
    let params = model["base"]["boot"]["kernel_params"]
        .as_array()
        .into_iter()
        .flatten();
    let params: Vec<&str> = params
        .map(|param| str_field(param, "kernel option"))
        .collect::<Result<_, _>>()?;
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
                // A program given by its path is used as it is. systemd looks for one given by
                // name only in its own fixed path, which has nothing of ours.
                if !program.is_empty() && !program.contains('/') {
                    let (_, inside) = packages
                        .iter()
                        .find(|(host, _)| host.join("bin").join(program).exists())
                        .ok_or_else(|| {
                            format!(
                                "`{key}` runs `{program}`, which none of the system's packages has"
                            )
                        })?;
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
    dir: &str,
) -> Result<Action<'a>, String> {
    let sections = &unit["sections"];
    let (unresolved, resolved) = action_records(&ids, packages, |deps| {
        json!({
            "kind": "unit", "name": name, "dir": dir, "sections": sections, "deps": deps,
        })
    })?;
    let name = name.to_owned();
    let dir = dir.to_owned();
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
                format!("{dir}/{name}"),
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
                            format!("{dir}/{target}.wants/{name}"),
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

/// Users' homes put in place at boot: their directories, what's kept of them and their files,
/// once /home is mounted, which may be a filesystem of its own, and before anyone logs in. The
/// first program does the rest before systemd starts, too early for /home.
const HOMES_UNIT: &str = "[Unit]\nDescription=Put users' homes in place\n\
    DefaultDependencies=no\nRequiresMountsFor=/home\nAfter=local-fs.target\n\
    Before=systemd-user-sessions.service shutdown.target\nConflicts=shutdown.target\n\n\
    [Service]\nType=oneshot\nRemainAfterExit=yes\n\
    ExecStart=/run/system-booted/bin/wrath activate --mode homes /run/system-booted\n";

/// What the first program sets up before systemd starts, set up again after a soft reboot, which
/// stops every unit, takes down the filesystems it made under /run/wrath and starts systemd again
/// without it. A boot that ran the first program has the wrappers' filesystem, so it skips this.
const SOFT_REBOOT_UNIT: &str = "[Unit]\nDescription=Set up again what boot sets up before systemd\n\
    DefaultDependencies=no\nConditionPathIsMountPoint=!/run/wrath/wrappers\nAfter=local-fs.target\n\
    Before=sysinit.target wrath-homes.service shutdown.target\nConflicts=shutdown.target\n\n\
    [Service]\nType=oneshot\nRemainAfterExit=yes\n\
    ExecStart=/run/system/bin/wrath activate --mode soft-reboot /run/system\n";
const SWITCH_TO: &str = include_str!("../system/switch-to-configuration");

/// The label of the file system a system's root is on.
pub const ROOT_LABEL: &str = "wrath-root";

/// What a generation keeps of its model, so wrath on the machine knows what it runs: wrath's
/// canonical text of it, and that text's hash.
pub struct Kept {
    pub text: String,
    pub hash: String,
    /// Each secret's encrypted file, by name, read from the configuration. Only the machine can
    /// decrypt them, so they go in the generation as they are.
    pub secrets: BTreeMap<String, Vec<u8>>,
}

#[cfg(test)]
impl Kept {
    /// For a test model that didn't come from wrath.
    pub fn plain(model: &Value) -> Self {
        let text = format!("{model}\n");
        Self {
            hash: digest(text.as_bytes()),
            text,
            secrets: BTreeMap::new(),
        }
    }
}

/// Where a generation keeps a secret's encrypted file, by the secret's name.
const SECRET_FILES: &str = "secrets";

/// What a generation says to fill in on the machine: each file's parts, by its name there.
const TEMPLATES_SPEC: &str = "templates.json";

/// Where activation fills in files with secrets or machine values.
pub const FILLED: &str = "/run/wrath/templates";

/// A filled-in file's name in [`FILLED`], from its path.
fn filled_name(path: &str) -> String {
    path.trim_start_matches('/').replace('/', "_")
}

/// Whether `model` has secrets, which need `age` on the machine.
fn has_secrets(model: &Value) -> bool {
    model["secrets"]
        .as_object()
        .is_some_and(|all| !all.is_empty())
}

/// A secret's or filled-in file's owner, as `owner` or `owner:group`, and its mode in octal.
fn owner_and_mode(entry: &Value) -> (String, String) {
    let owner = entry["owner"].as_str().unwrap_or("root");
    let owner = match entry["group"].as_str() {
        Some(group) => format!("{owner}:{group}"),
        None => owner.to_owned(),
    };
    let mode = entry["mode"].as_u64().unwrap_or(0o400);
    (owner, format!("{mode:04o}"))
}

/// What wrath on the machine reads at every boot and switch: files under /etc with a mode of
/// their own, which are copied instead of linked, the homes whose files it links, and the work it
/// does itself: secrets to decrypt, with what and with which keys, files to fill in with them or
/// with the machine's values, and whether the hostname is one of those.
fn activation(model: &Value) -> String {
    let homes: Vec<Value> = model["homes"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, home)| {
            let has = |key: &str| home[key].as_object().is_some_and(|all| !all.is_empty());
            has("files") || has("units")
        })
        .map(|(name, _)| {
            let user = &model["base"]["users"][name];
            json!({"name": name, "group": user["group"], "home": user["home"]})
        })
        .collect();
    let mut modes = Map::new();
    for (path, file) in model["files"].as_object().into_iter().flatten() {
        let mode = file["mode"].as_u64().unwrap_or(0o644);
        if let Some(inside) = path.strip_prefix("/etc/")
            && mode != 0o644
        {
            modes.insert(inside.to_owned(), json!(format!("{mode:04o}")));
        }
    }
    let mut secrets = Map::new();
    for (name, secret) in model["secrets"].as_object().into_iter().flatten() {
        let (owner, mode) = owner_and_mode(secret);
        secrets.insert(
            name.clone(),
            json!({
                "source": format!("{SECRET_FILES}/{name}.age"),
                "owner": owner,
                "mode": mode,
                "link": secret["link"].as_str().unwrap_or(""),
            }),
        );
    }
    let templates: Vec<Value> = model["templates"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(path, template)| {
            let (owner, mode) = owner_and_mode(template);
            json!({"name": filled_name(path), "owner": owner, "mode": mode})
        })
        .collect();
    json!({
        "etc_modes": modes,
        "bash": "@bash@/bin/bash",
        "age": if has_secrets(model) { "@age@/bin/age" } else { "" },
        "identities": model["base"]["secrets"]["identities"].as_array().cloned().unwrap_or_default(),
        "secrets": secrets,
        "templates": templates,
        "templates_spec": TEMPLATES_SPEC,
        "hostname_filled": model["templates"].get("/etc/hostname").is_some(),
        "homes": homes,
    })
    .to_string()
}

/// The encrypted secrets, as they are, and the parts of every file the machine fills in, with each
/// such file under /etc linked to where activation fills it in.
fn runtime_files<'a>(
    model: &Value,
    secrets: BTreeMap<String, Vec<u8>>,
    packages: &'a Packages,
) -> Result<Action<'a>, String> {
    let spec: Map<String, Value> = model["templates"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(path, template)| (filled_name(path), template["parts"].clone()))
        .collect();
    let links: Vec<(String, String)> = model["templates"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(path, _)| {
            let inside = path.strip_prefix('/')?;
            inside
                .starts_with("etc/")
                .then(|| (inside.to_owned(), format!("{FILLED}/{}", filled_name(path))))
        })
        .collect();
    let hashes: Map<String, Value> = secrets
        .iter()
        .map(|(name, bytes)| (name.clone(), json!(digest(bytes))))
        .collect();
    let spec = Value::Object(spec);
    let (unresolved, resolved) = action_records(&[], packages, |deps| {
        json!({"kind": "runtime-files", "secrets": hashes, "templates": spec,
            "links": links, "deps": deps})
    })?;
    Ok(Action {
        origin: "secrets".to_owned(),
        unresolved,
        resolved,
        run: Box::new(move |objects| {
            let mut entries = Entries::new();
            for (name, bytes) in &secrets {
                let object = objects.put(bytes)?;
                let path = format!("{SECRET_FILES}/{name}.age");
                entries.insert(
                    path,
                    Entry::File {
                        mode: 0o444,
                        object,
                    },
                );
            }
            let text = serde_json::to_string_pretty(&spec).map_err(|e| e.to_string())?;
            let object = objects.put(text.as_bytes())?;
            entries.insert(
                TEMPLATES_SPEC.to_owned(),
                Entry::File {
                    mode: 0o444,
                    object,
                },
            );
            for (path, target) in &links {
                entries.insert(
                    path.clone(),
                    Entry::Link {
                        target: target.clone(),
                    },
                );
            }
            Ok(entries)
        }),
    })
}

/// Turns a model into actions. Every package it names must have a recipe.
fn lower<'a>(
    lab: &'a Lab,
    model: &'a Value,
    kept: &Kept,
    packages: &'a Packages,
) -> Result<Lowered<'a>, String> {
    let (named, mut skipped) = model_packages(model, |id| packages.contains_key(id))?;
    // The kernel, its firmware and microcode are packages too, named where the kernel is set up.
    let kernel = &model["base"]["kernel"];
    let mut missing = |name: &str| {
        if !skipped.iter().any(|seen| seen == name) {
            skipped.push(name.to_owned());
        }
    };
    // There's one kernel recipe so far.
    if let Some(other) = kernel["package"].as_str().filter(|name| *name != "linux") {
        missing(other);
    }
    let mut firmware = Vec::new();
    for name in firmware_ids(model) {
        if packages.contains_key(name) {
            firmware.push(name);
        } else {
            missing(name);
        }
    }
    if let Some(name) = kernel["microcode"]["package"].as_str()
        && !packages.contains_key(name)
    {
        missing(name);
    }
    let everything: Vec<&'a str> = {
        let mut all: BTreeSet<&'a str> = MECHANISM_PACKAGES.iter().copied().collect();
        all.extend(named.iter().copied());
        all.into_iter().collect()
    };

    let sharing = sharing(lab, &everything, packages)?;

    let base = "base";
    // Packages whose units, udev rules, D-Bus services and the like join the system, not only
    // their programs: systemd and dbus, then those the model names.
    let mut services: Vec<&'a str> = vec!["systemd", "dbus"];
    for value in model["service_packages"].as_array().into_iter().flatten() {
        if let Some(name) = value.as_str()
            && packages.contains_key(name)
            && !services.contains(&name)
        {
            services.push(name);
        }
    }
    // The buses' service directories, system and session, of every service package.
    let service_dirs = |dir: &str| -> String {
        services
            .iter()
            .map(|id| format!("<servicedir>@{id}@/share/dbus-1/{dir}</servicedir>"))
            .collect()
    };
    let (system_services, session_services) =
        (service_dirs("system-services"), service_dirs("services"));
    let hostname = str_field(&model["base"]["system"]["hostname"], "hostname")?;
    let mut actions = Vec::new();
    // What a system calls itself, unless its modules say, as wrath's `base-system` does.
    if model["files"].get("/etc/os-release").is_none() {
        actions.push(text(
            base,
            "etc/os-release",
            0o644,
            "NAME=wrath\nID=wrath\nPRETTY_NAME=wrath\n".into(),
            &[],
            packages,
        )?);
    }
    actions.extend([
        text(
            "base.users",
            "etc/tmpfiles.d/wrath-homes.conf",
            0o644,
            homes(model)?,
            &[],
            packages,
        )?,
        text(
            "base.users",
            "etc/systemd/system/wrath-homes.service",
            0o644,
            HOMES_UNIT.to_owned(),
            &[],
            packages,
        )?,
        text(
            base,
            "etc/systemd/system/wrath-soft-reboot.service",
            0o644,
            SOFT_REBOOT_UNIT.to_owned(),
            &[],
            packages,
        )?,
        text(
            base,
            "etc/dbus-1/system.d/services.conf",
            0o644,
            format!("<busconfig>{system_services}</busconfig>\n"),
            &services,
            packages,
        )?,
        text(
            base,
            "etc/dbus-1/session.d/services.conf",
            0o644,
            format!("<busconfig>{session_services}</busconfig>\n"),
            &services,
            packages,
        )?,
        text(
            base,
            "etc/systemd/system.conf",
            0o644,
            "[Manager]\nDefaultEnvironment=PATH=/run/wrath/wrappers/bin:/run/system/bin\n".into(),
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
            "etc/wrath/model.json",
            0o644,
            kept.text.clone(),
            &[],
            packages,
        )?,
        text(
            "model",
            "etc/wrath/model-hash",
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
            if has_secrets(model) {
                &["bash", "age"]
            } else {
                &["bash"]
            },
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
            &[
                ("bin", "bin"),
                // Some packages put programs for root in sbin, which is on the path all the same.
                ("sbin", "bin"),
                ("lib/security", "lib/security"),
                // NSS modules, which glibc looks for there when its own search finds none.
                ("lib/nss", "lib/nss"),
                // Compiled locales, which LOCPATH names.
                ("lib/locale", "lib/locale"),
            ],
            packages,
        )?,
        links(
            "packages",
            lab,
            sharing.clone(),
            &[("share", "share")],
            packages,
        )?,
        links(base, lab, services.clone(), &system_layout(), packages)?,
        // systemd finds its own units; the others' join the system's.
        links(
            "services",
            lab,
            services[1..].to_vec(),
            &[
                ("lib/systemd/system", SYSTEM_UNITS),
                ("lib/systemd/user", SYSTEM_USER_UNITS),
            ],
            packages,
        )?,
        fixed_links(packages)?,
        localtime(
            model["base"]["system"]["timezone"]
                .as_str()
                .unwrap_or("UTC"),
            packages,
        )?,
    ]);
    actions.push(grapes_link(packages)?);
    // A hostname filled in from the machine's name is one of the files the machine fills in.
    if !hostname.is_empty() {
        actions.push(text(
            base,
            "etc/hostname",
            0o644,
            format!("{hostname}\n"),
            &[],
            packages,
        )?);
    }
    actions.extend(index_actions("packages", lab, &sharing, packages)?);
    if has_secrets(model)
        || model["templates"]
            .as_object()
            .is_some_and(|all| !all.is_empty())
    {
        actions.push(runtime_files(model, kept.secrets.clone(), packages)?);
    }

    let disks = early::disks(&model["base"]["storage"]);
    for (path, contents) in [("etc/fstab", disks.fstab), ("etc/crypttab", disks.crypttab)] {
        if !contents.is_empty() {
            actions.push(text("base.storage", path, 0o644, contents, &[], packages)?);
        }
    }
    // What the initrd takes from the machine, as credentials the boot loader passes it: each
    // credential's name and the file it's made from.
    if disks.ssh.is_some() {
        actions.push(text(
            "base.storage",
            "initrd-credentials",
            0o644,
            format!("{INITRD_SSH_CREDENTIAL} {INITRD_SSH_KEY}\n"),
            &[],
            packages,
        )?);
    }
    let mut outside = String::new();
    for (path, file) in model["files"].as_object().ok_or("the model has no files")? {
        let at = installed_at(path);
        if at.starts_with("files/") {
            outside.push_str(&format!("L+ {path} - - - - /run/system/{at}\n"));
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
        "etc/tmpfiles.d/wrath-files.conf",
        0o644,
        outside,
        &[],
        packages,
    )?);
    for (name, unit) in model["units"].as_object().ok_or("the model has no units")? {
        actions.push(unit_action(
            lab,
            name,
            unit,
            named.clone(),
            packages,
            SYSTEM_UNITS,
        )?);
    }
    for (user, home) in model["homes"].as_object().into_iter().flatten() {
        actions.extend(home_actions(
            lab,
            user,
            home,
            &named,
            &mut skipped,
            packages,
        )?);
    }
    let (enabled, unfound) = enabled_units(lab, model, &everything, packages)?;
    actions.push(enabled);
    actions.push(boot_files(lab, model, packages)?);
    if !firmware.is_empty() {
        actions.push(firmware_links(lab, firmware, packages)?);
    }
    let foreign: Vec<&str> = model["base"]["foreign"]["libraries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !foreign.is_empty() {
        actions.push(foreign_files(lab, foreign, packages)?);
    }
    let keys = built_from(&actions, packages);
    actions.push(text("model", PACKAGE_KEYS, 0o644, keys, &[], packages)?);
    Ok(Lowered {
        actions,
        skipped,
        unfound,
    })
}

/// Links that pull in the model's enabled units, the system's and users': into each target it
/// names, or into those its `[Install]` section names when it names none. Also returns the units
/// no package has.
fn enabled_units<'a>(
    lab: &'a Lab,
    model: &'a Value,
    present: &[&'a str],
    packages: &'a Packages,
) -> Result<(Action<'a>, Vec<String>), String> {
    // systemd's and dbus's own units first, then those of every other package the system has.
    let first = ["systemd", "dbus"];
    let mut ids: Vec<&'a str> = first.to_vec();
    ids.extend(present.iter().filter(|id| !first.contains(id)));
    let mut links = BTreeMap::new();
    let mut missing = Vec::new();
    for kind in &UNIT_KINDS {
        for (unit, targets) in model[kind.enabled].as_object().into_iter().flatten() {
            let file = format!("{}/{unit}", kind.packaged);
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
                    format!("{}/{target}.wants/{unit}", kind.added),
                    inside.clone(),
                );
            }
            // systemd finds a wanted unit by its name on its search path, which may have the
            // package's units already.
            if !kind.found.contains(&id) {
                links.insert(format!("{}/{unit}", kind.added), inside);
            }
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
            // Only plain names: `/` or `..` would link from outside the target's directory.
            targets.extend(
                names
                    .split_whitespace()
                    .filter(|name| !name.contains('/') && !name.starts_with('.'))
                    .map(str::to_owned),
            );
        }
    }
    targets
}

/// Where a generation says what it was built from: the key each package it uses has before
/// anything is built, by package. A plan compares them with today's recipes to say what a newer
/// wrath rebuilds.
pub const PACKAGE_KEYS: &str = "etc/wrath/packages.json";

/// The packages `actions` use, with their keys, as [`PACKAGE_KEYS`] holds them.
fn built_from(actions: &[Action<'_>], packages: &Packages) -> String {
    let used: BTreeMap<&str, &str> = actions
        .iter()
        .filter_map(|action| action.unresolved["deps"].as_object())
        .flat_map(Map::keys)
        .filter_map(|id| Some((id.as_str(), packages.get(id)?.unresolved.as_str())))
        .collect();
    let mut text = serde_json::to_string_pretty(&used).expect("a map of strings serializes");
    text.push('\n');
    text
}

/// A model's actions, the packages it names that no recipe builds, and the units it enables that
/// no package it has provides.
struct Lowered<'a> {
    actions: Vec<Action<'a>>,
    skipped: Vec<String>,
    unfound: Vec<String>,
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
        if known(name) {
            if !named.contains(&name) {
                named.push(name);
            }
        } else if !skipped.iter().any(|seen| seen == name) {
            skipped.push(name.to_owned());
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

/// The packages in the initrd of a system with `disks`: openssh too, for unlocking over SSH.
fn initrd_packages(disks: &early::Disks) -> Vec<&'static str> {
    let mut ids = INITRD_PACKAGES.to_vec();
    if disks.ssh.is_some() {
        ids.push("openssh");
    }
    ids
}

/// device-mapper's udev rules, which name unlocked containers so they can be mounted.
const DM_RULES: [&str; 3] = ["10-dm.rules", "13-dm-disk.rules", "95-dm-notify.rules"];

const INITRD_RELEASE: &str = "NAME=wrath\nID=wrath\nPRETTY_NAME=\"wrath (initrd)\"\n";

/// The initrd's own files, by path, where `@id@` stands for package `id`'s path: systemd's
/// configuration, accounts, how to unlock and mount the disks, and the service that readies the
/// root.
fn initrd_files(
    disks: &early::Disks,
    hostname: &str,
    modprobe: &str,
) -> Vec<(String, u32, String)> {
    let unit = "[Unit]\nDescription=Ready the root to switch to\nDefaultDependencies=no\n\
        RequiresMountsFor=/sysroot /sysroot/wrath\nAfter=initrd-fs.target\n\
        Before=initrd.target\n\n[Service]\nType=oneshot\nRemainAfterExit=yes\n\
        ExecStart=/etc/wrath-root\n";
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
    // sshd separates privileges as its own user, and networkd runs as one.
    let (users, user_groups) = if disks.ssh.is_some() {
        (
            "sshd:x:74:74::/var/empty:/bin/nologin\n\
             systemd-network:x:152:152::/:/bin/nologin\n",
            "sshd:x:74:\nsystemd-network:x:152:\n",
        )
    } else {
        ("", "")
    };
    let mut files = vec![
        file("etc/initrd-release", 0o644, INITRD_RELEASE.to_owned()),
        file("etc/os-release", 0o644, INITRD_RELEASE.to_owned()),
        file("etc/systemd/system.conf", 0o644, system_conf.to_owned()),
        file(
            "etc/systemd/system/wrath-root.service",
            0o644,
            unit.to_owned(),
        ),
        file(
            "etc/systemd/system/emergency.service.d/wrath.conf",
            0o644,
            force_shell.to_owned(),
        ),
        file(
            "etc/systemd/system/rescue.service.d/wrath.conf",
            0o644,
            force_shell.to_owned(),
        ),
        file("etc/wrath-root", 0o755, INITRD_ROOT.to_owned()),
        file("etc/fstab", 0o644, disks.initrd_fstab.clone()),
        file("etc/crypttab", 0o644, disks.crypttab.clone()),
        file(
            "etc/passwd",
            0o644,
            format!("root:x:0:0:root:/root:/bin/sh\n{users}"),
        ),
        file(
            "etc/group",
            0o644,
            format!("root:x:0:\n{groups}{user_groups}"),
        ),
        file("etc/shadow", 0o600, "root:*:::::::\n".to_owned()),
        file("etc/modules-load.d/wrath.conf", 0o644, modules),
    ];
    if !hostname.is_empty() {
        files.push(file("etc/hostname", 0o644, format!("{hostname}\n")));
    }
    if let Some(ssh) = &disks.ssh {
        files.extend(initrd_ssh_files(ssh));
    }
    // Options for modules, which those loaded in the initrd need too.
    if !modprobe.is_empty() {
        files.push(file(
            "etc/modprobe.d/wrath.conf",
            0o644,
            modprobe.to_owned(),
        ));
    }
    files
}

/// The credential the initrd's SSH server takes its host key from: the machine's initrd key,
/// sealed to its TPM where it has one, next to the boot loader.
pub const INITRD_SSH_CREDENTIAL: &str = "wrath.initrd-ssh-key";

/// Where a machine keeps the key the initrd's SSH server presents.
pub const INITRD_SSH_KEY: &str = "/etc/ssh/initrd_ed25519_key";

/// The initrd's files for SSH: a server on `ssh`'s port that lets its keys in as root to type the
/// disks' passphrase, with its host key from a credential, and DHCP on every wired network.
fn initrd_ssh_files(ssh: &early::Ssh) -> Vec<(String, u32, String)> {
    // Every file in the initrd is wrath's, and its root is writable by more than root, which
    // sshd's checks of the authorized keys' directories would refuse.
    let config = format!(
        "Port {}\nPermitRootLogin prohibit-password\nPasswordAuthentication no\n\
         KbdInteractiveAuthentication no\nUsePAM no\nStrictModes no\n\
         AuthorizedKeysFile /etc/ssh/authorized_keys.d/%u\n",
        ssh.port
    );
    let keys: String = ssh
        .authorized_keys
        .iter()
        .map(|key| format!("{key}\n"))
        .collect();
    let unit = format!(
        "[Unit]\nDescription=SSH to unlock the disks\nDefaultDependencies=no\n\
         Wants=systemd-networkd.service\nAfter=systemd-networkd.service\n\n\
         [Service]\nLoadCredentialEncrypted={INITRD_SSH_CREDENTIAL}\n\
         ExecStart=/bin/sshd -D -e -f /etc/ssh/sshd_config -h %d/{INITRD_SSH_CREDENTIAL}\n"
    );
    let network = "[Match]\nType=ether\n\n[Network]\nDHCP=yes\n";
    [
        ("etc/ssh/sshd_config", config),
        ("etc/ssh/authorized_keys.d/root", keys),
        ("etc/systemd/system/wrath-initrd-ssh.service", unit),
        ("etc/systemd/network/10-wired.network", network.to_owned()),
    ]
    .into_iter()
    .map(|(path, text)| (path.to_owned(), 0o644, text))
    .collect()
}

/// A modprobe deny-list for the initrd: the modules in `tree` that read firmware, themselves or
/// through a module they depend on, and `wanted` doesn't need. Its udev would otherwise load them
/// for devices it finds before the root, which holds the firmware, is mounted, and a driver that
/// fails to set up its device there doesn't try again. Other drivers, such as the root disk's,
/// still load for their devices, and loading a module by name, as for `wanted`, still works.
fn initrd_deny_list(tree: &Path, wanted: &[String]) -> String {
    let name = |path: &str| {
        let file = path.rsplit('/').next().unwrap_or_default();
        file.split(".ko")
            .next()
            .unwrap_or_default()
            .replace('-', "_")
    };
    let mut deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut firmware = BTreeSet::new();
    for version in fs::read_dir(tree).into_iter().flatten().flatten() {
        let dir = version.path();
        let text = fs::read_to_string(dir.join("modules.dep")).unwrap_or_default();
        for line in text.lines() {
            let (path, needs) = line.split_once(':').unwrap_or((line, ""));
            let module = name(path);
            if reads_firmware(&dir.join(path)) {
                firmware.insert(module.clone());
            }
            deps.insert(module, needs.split_whitespace().map(name).collect());
        }
    }
    let closure = |start: Vec<String>| {
        let mut seen = BTreeSet::new();
        let mut pending = start;
        while let Some(module) = pending.pop() {
            if seen.insert(module.clone()) {
                pending.extend(deps.get(&module).into_iter().flatten().cloned());
            }
        }
        seen
    };
    let needed = closure(wanted.iter().map(|m| m.replace('-', "_")).collect());
    // A module whose dependencies read firmware reads it too: blocking only the one that names it
    // doesn't stop it loading as a dependency.
    deps.keys()
        .filter(|module| !needed.contains(*module))
        .filter(|module| !closure(vec![(*module).clone()]).is_disjoint(&firmware))
        .map(|module| format!("blacklist {module}\n"))
        .collect()
}

/// Whether the module at `path` names firmware it loads, as a `firmware=` entry of its modinfo.
/// Compressed modules aren't read, and count as not reading any.
fn reads_firmware(path: &Path) -> bool {
    fs::read(path).is_ok_and(|bytes| bytes.windows(10).any(|w| w == b"\0firmware="))
}

/// The modules of `wanted` that the kernel whose modules are in `tree` has, as a module or built
/// in. Names match with `-` and `_` alike, as kmod matches them.
fn kernel_has(tree: &Path, wanted: &[String]) -> Vec<String> {
    let mut known = BTreeSet::new();
    for version in fs::read_dir(tree).into_iter().flatten().flatten() {
        for list in ["modules.builtin", "modules.dep"] {
            let text = fs::read_to_string(version.path().join(list)).unwrap_or_default();
            for line in text.lines() {
                let path = line.split(':').next().unwrap_or_default();
                let file = path.rsplit('/').next().unwrap_or_default();
                let name = file.split(".ko").next().unwrap_or_default();
                known.insert(name.replace('-', "_"));
            }
        }
    }
    wanted
        .iter()
        .filter(|module| known.contains(&module.replace('-', "_")))
        .cloned()
        .collect()
}

/// Where a program built elsewhere asks for its loader, under the generation (decision 0015).
const FOREIGN_LOADER: &str = "lib64/ld-linux-x86-64.so.2";
/// The library cache that loader reads, under the generation.
const FOREIGN_CACHE: &str = "foreign/ld.so.cache";

/// glibc's loader with the cache it reads moved to `/run/ld.so.cache`, which activation links to
/// the running generation's. The path has the same length, so nothing else in the file moves.
fn foreign_loader(loader: &[u8]) -> Result<Vec<u8>, String> {
    const FROM: &[u8] = b"/etc/ld.so.cache\0";
    const TO: &[u8] = b"/run/ld.so.cache\0";
    let mut found = memchr::memmem::find_iter(loader, FROM);
    let (Some(at), None) = (found.next(), found.next()) else {
        return Err("glibc's loader doesn't name `/etc/ld.so.cache` once".to_owned());
    };
    let mut patched = loader.to_vec();
    patched[at..at + TO.len()].copy_from_slice(TO);
    Ok(patched)
}

/// The loader and library cache for programs built elsewhere, from glibc and the libraries of
/// `ids`, made with glibc's `ldconfig`.
fn foreign_files<'a>(
    lab: &'a Lab,
    ids: Vec<&'a str>,
    packages: &'a Packages,
) -> Result<Action<'a>, String> {
    let mut deps = vec!["glibc"];
    deps.extend(ids.iter().filter(|id| **id != "glibc"));
    let (unresolved, resolved) = action_records(
        &deps,
        packages,
        |deps| json!({"kind": "foreign", "deps": deps, "order": ids}),
    )?;
    let key = key_of(&resolved);
    Ok(Action {
        origin: "programs.foreign".to_owned(),
        unresolved,
        resolved,
        run: Box::new(move |objects| {
            let glibc = &package(packages, "glibc")?.path;
            let from = lab.on_host(glibc).join("lib/ld-linux-x86-64.so.2");
            let loader = foreign_loader(&fs::read(&from).map_err(with_path(&from))?)?;
            let dir = lab.work.join("foreign").join(&key);
            chain::fresh(&dir)?;
            let mut inputs = chain::runtime_closure(lab, glibc)?;
            let mut conf = String::new();
            for id in &ids {
                let package = package(packages, id)?;
                for path in std::iter::once(&package.path).chain(&package.outputs) {
                    if lab.on_host(path).join("lib").is_dir() {
                        conf.push_str(&format!("{}/lib\n", path.inside()));
                        inputs.extend(chain::runtime_closure(lab, path)?);
                    }
                }
            }
            let build = dir.join("build");
            fs::create_dir_all(&build).map_err(with_path(&build))?;
            fs::write(build.join("ld.so.conf"), conf).map_err(with_path(&build))?;
            let log = dir.join("log");
            let argv = vec![
                format!("{}/bin/ldconfig", glibc.inside()),
                "-X".to_owned(),
                "-f".to_owned(),
                "ld.so.conf".to_owned(),
                "-C".to_owned(),
                "ld.so.cache".to_owned(),
            ];
            let outcome = sandbox::run(
                lab,
                &sandbox::Step {
                    name: "foreign libraries".to_owned(),
                    argv,
                    inputs: inputs.into_iter().collect(),
                    output: None,
                    view: None,
                    build_dir: build.clone(),
                    log: log.clone(),
                    env: BTreeMap::new(),
                    limits: sandbox::Limits::DEFAULT,
                    links: Vec::new(),
                    at: None,
                },
            )?;
            if outcome.status != sandbox::Status::Passed {
                let text = fs::read_to_string(&log).unwrap_or_default();
                return Err(format!(
                    "`ldconfig` failed for the foreign libraries:\n{text}"
                ));
            }
            let cache = build.join("ld.so.cache");
            let cache = fs::read(&cache).map_err(with_path(&cache))?;
            let entries = Entries::from([
                (
                    FOREIGN_LOADER.to_owned(),
                    Entry::File {
                        mode: 0o755,
                        object: objects.put(&loader)?,
                    },
                ),
                (
                    FOREIGN_CACHE.to_owned(),
                    Entry::File {
                        mode: 0o644,
                        object: objects.put(&cache)?,
                    },
                ),
            ]);
            chain::remove(&dir)?;
            Ok(entries)
        }),
    })
}

/// The kernel and an initrd that reaches the root: systemd, which unlocks the disks the model
/// describes, mounts the root under /sysroot and switches to the `init=` on the kernel's command
/// line. Without disks, the root is the one `root=` names.
fn boot_files<'a>(
    lab: &'a Lab,
    model: &'a Value,
    packages: &'a Packages,
) -> Result<Action<'a>, String> {
    let kernel = &model["base"]["kernel"];
    let mut disks = early::disks(&model["base"]["storage"]);
    // The configuration's own initrd modules, after what the disks need.
    for module in kernel["initrd_modules"].as_array().into_iter().flatten() {
        if let Some(module) = module.as_str()
            && !disks.modules.iter().any(|m| m == module)
        {
            disks.modules.push(module.to_owned());
        }
    }
    let hostname = model["base"]["system"]["hostname"]
        .as_str()
        .unwrap_or_default();
    let modprobe = kernel["modprobe"].as_str().unwrap_or_default();
    let files = initrd_files(&disks, hostname, modprobe);
    // CPU microcode, from a package with it at `kernel/x86/microcode/`.
    let microcode = kernel["microcode"]["package"]
        .as_str()
        .filter(|id| packages.contains_key(*id));
    let initrd_packages = initrd_packages(&disks);
    let mut ids = [initrd_packages.as_slice(), &["linux"]].concat();
    ids.extend(microcode);
    let (unresolved, resolved) = action_records(&ids, packages, |deps| {
        json!({"kind": "boot", "files": files, "rules": DM_RULES,
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
                "etc/systemd/system/initrd.target.requires/wrath-root.service",
                link("../wrath-root.service".to_owned()),
            );
            if disks.ssh.is_some() {
                initrd::insert(&mut nodes, "var/empty", initrd::Node::Dir);
                let wants = "etc/systemd/system/initrd.target.wants";
                initrd::insert(
                    &mut nodes,
                    &format!("{wants}/systemd-networkd.service"),
                    link(format!(
                        "{systemd}/lib/systemd/system/systemd-networkd.service"
                    )),
                );
                initrd::insert(
                    &mut nodes,
                    &format!("{wants}/wrath-initrd-ssh.service"),
                    link("../wrath-initrd-ssh.service".to_owned()),
                );
            }
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
            // Every program of the packages on the path, the way a system's `bin` has them.
            let mut roots = Vec::new();
            for id in &initrd_packages {
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
                let to = format!("wrath/store/{}/lib/modules", linux.hash());
                initrd::add_tree(&mut nodes, &modules, &to)?;
                let target = format!("{}/lib/modules", linux.inside());
                initrd::insert(&mut nodes, "lib/modules", link(target));
            }
            // Only what this kernel has, as a module or built in, so none of them fails to load.
            let loaded: String = kernel_has(&modules, &disks.modules)
                .iter()
                .map(|module| format!("{module}\n"))
                .collect();
            let bytes = loaded.into_bytes();
            initrd::insert(
                &mut nodes,
                "etc/modules-load.d/wrath.conf",
                initrd::Node::File { mode: 0o644, bytes },
            );
            let bytes = initrd_deny_list(&modules, &disks.modules).into_bytes();
            initrd::insert(
                &mut nodes,
                "etc/modprobe.d/initrd.conf",
                initrd::Node::File { mode: 0o644, bytes },
            );
            // Roots come from `root=` or the fstab, never from a partition marked as one.
            initrd::insert(
                &mut nodes,
                "etc/systemd/system-generators/systemd-gpt-auto-generator",
                link("/dev/null".to_owned()),
            );
            let mut compressed = Vec::new();
            // The CPU reads its microcode from an uncompressed archive at the very start.
            if let Some(id) = microcode {
                let mut early = initrd::Nodes::new();
                let from = lab.on_host(&package(packages, id)?.path).join("kernel");
                initrd::add_tree(&mut early, &from, "kernel")?;
                compressed = initrd::archive(&early);
            }
            compressed.extend(
                zstd::bulk::compress(&initrd::archive(&nodes), 3)
                    .map_err(|e| format!("compressing the initrd: {e}"))?,
            );
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

/// The packages links can point into, by `@id@`.
const LINKED: [&str; 2] = ["systemd", "linux"];

/// Links every system has, where `@id@` stands for the path of package `id`, one of [`LINKED`].
/// They're part of the action's key, so changing one changes the action.
const FIXED_LINKS: [(&str, &str); 8] = [
    // wrath starts systemd from here at boot.
    ("systemd", "@systemd@"),
    // The modules of the kernel, which wrath links at /lib/modules when the system boots.
    ("kernel-modules", "@linux@"),
    (
        "etc/systemd/system/sockets.target.wants/dbus.socket",
        "../dbus.socket",
    ),
    // Users' homes, once /home is mounted.
    (
        "etc/systemd/system/multi-user.target.wants/wrath-homes.service",
        "../wrath-homes.service",
    ),
    // What boot sets up before systemd, again after a soft reboot.
    (
        "etc/systemd/system/sysinit.target.wants/wrath-soft-reboot.service",
        "../wrath-soft-reboot.service",
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

/// Where `/etc/localtime` points for the time zone `zone`, such as `Europe/Stockholm`: into the
/// zones the running system shares from `tzdata`, where glibc, systemd and `timedatectl` read it.
fn zone_file(zone: &str) -> Result<String, String> {
    let plain = !zone.is_empty()
        && zone.split('/').all(|part| {
            !part.is_empty()
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_+-".contains(&b))
        });
    if !plain {
        return Err(format!(
            "`{zone}` isn't a time zone: name one as tzdata does, such as `Europe/Stockholm`"
        ));
    }
    Ok(format!("/run/system/share/zoneinfo/{zone}"))
}

/// The package of the set the system was built from, which it links as `grapes`, so the machine
/// evaluates its configuration with it, offline, while the lock pins that version.
const GRAPES: &str = "grapes";

/// The `grapes` link, to the set the system was built from.
fn grapes_link(packages: &Packages) -> Result<Action<'_>, String> {
    let (unresolved, resolved) = action_records(
        &[GRAPES],
        packages,
        |deps| json!({"kind": "grapes", "deps": deps}),
    )?;
    let target = package(packages, GRAPES)?.path.inside();
    Ok(Action {
        origin: "base.system".to_owned(),
        unresolved,
        resolved,
        run: Box::new(move |_| {
            let link = Entry::Link {
                target: target.clone(),
            };
            Ok(Entries::from([(GRAPES.to_owned(), link)]))
        }),
    })
}

/// `/etc/localtime`, linking to the configuration's time zone.
fn localtime<'a>(zone: &str, packages: &'a Packages) -> Result<Action<'a>, String> {
    let target = zone_file(zone)?;
    let (unresolved, resolved) = action_records(
        &[],
        packages,
        |deps| json!({"kind": "localtime", "zone": zone, "deps": deps}),
    )?;
    Ok(Action {
        origin: "base.system".to_owned(),
        unresolved,
        resolved,
        run: Box::new(move |_| {
            let link = Entry::Link {
                target: target.clone(),
            };
            Ok(Entries::from([("etc/localtime".to_owned(), link)]))
        }),
    })
}

fn fixed_links(packages: &Packages) -> Result<Action<'_>, String> {
    links_to("base", &FIXED_LINKS, packages)
}

/// Links, each from a path in the generation to a target where `@id@` stands for the path of
/// package `id`, one of [`LINKED`]. They're part of the action's key.
fn links_to<'a>(
    origin: &str,
    links: &'static [(&'static str, &'static str)],
    packages: &'a Packages,
) -> Result<Action<'a>, String> {
    let (unresolved, resolved) = action_records(
        &LINKED,
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
                    let target = fill(target, &LINKED, packages)?;
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
    /// What each action made, by its resolved key, as the action database keeps it.
    pub outputs: BTreeMap<String, String>,
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
    let mut outputs: BTreeMap<String, String> = BTreeMap::new();
    let (mut unresolved_parts, mut resolved_parts) = (Map::new(), Map::new());
    for action in actions {
        let (unresolved, resolved) = (key_of(&action.unresolved), key_of(&action.resolved));
        put_action(&tx, &unresolved, &action.unresolved)?;
        put_action(&tx, &resolved, &action.resolved)?;
        // A machine installed from an image has what its actions made beside its store, and no
        // database yet. An output whose objects are gone, as after a crash, is made again.
        let cached = output_of(&tx, &resolved)?
            .or_else(|| crate::records::action(lab, &resolved))
            .filter(|text| {
                serde_json::from_str::<Entries>(text).is_ok_and(|entries| objects.hold(&entries))
            });
        let text = match cached {
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
        outputs.insert(resolved.clone(), text.clone());
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
    // A generation's files can't name it, so its name follows from them.
    let contents = serde_json::to_string(&merged).map_err(|e| e.to_string())?;
    let name = format!("{}-system", digest(contents.as_bytes()));
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
        outputs,
    })
}

fn objects_of(lab: &Lab) -> Objects {
    Objects(lab.work.join("e2e/objects"))
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
                    indexes: built.indexes.get(id).cloned().unwrap_or_default(),
                    outputs: match built.outputs.get(id) {
                        Some(outputs) => [split::Class::Out, split::Class::Lib]
                            .iter()
                            .filter_map(|class| outputs.get(class).cloned())
                            .collect(),
                        None => vec![path.clone()],
                    },
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

/// Lines about recipe files that aren't where anyone would look for them: each is named after
/// one of its packages, under the first two letters of that name, and declares ids no other file
/// does.
pub fn misplaced_recipes(repository: &Path) -> Result<String, String> {
    let mut lines = String::new();
    let mut declared: BTreeMap<String, String> = BTreeMap::new();
    for file in recipe_files(repository)? {
        let packages = chain::load(repository, &file)?;
        let (shard, name) = file.rsplit_once('/').unwrap_or(("", &file));
        let name = name.strip_suffix(".toml").unwrap_or(name);
        if !packages.iter().any(|package| package.id == name) {
            lines.push_str(&format!(
                "recipes/{file}: named after none of its packages; name it after the main one\n"
            ));
        }
        let wanted: String = name
            .chars()
            .take(2)
            .collect::<String>()
            .to_ascii_lowercase();
        if shard != wanted {
            lines.push_str(&format!(
                "recipes/{file}: goes in recipes/{wanted}/, under the first letters of its name\n"
            ));
        }
        for package in packages {
            if let Some(other) = declared.insert(package.id.clone(), file.clone()) {
                lines.push_str(&format!(
                    "`{}` is declared in recipes/{other} and recipes/{file}\n",
                    package.id
                ));
            }
        }
    }
    Ok(lines)
}

/// Lines of recipes and styles under `repository` that write a target's name, such as `x86_64`,
/// where a placeholder or variable says it for whichever target builds (0032): `{arch}`,
/// `{triple}`, `$WRATH_ARCH`, `$WRATH_TRIPLE` or `$WRATH_LOADER`. Comments don't count.
pub fn targets_written(repository: &Path) -> Result<String, String> {
    // How projects spell a target their own way too: Go's `amd64`, gcc's `-march=x86-64`.
    const NAMES: [&str; 6] = ["x86_64", "aarch64", "amd64", "x86-64", "x64", "arm64"];
    let mut files: Vec<PathBuf> = recipe_files(repository)
        .unwrap_or_default()
        .iter()
        .map(|file| repository.join("recipes").join(file))
        .collect();
    if let Ok(entries) = std::fs::read_dir(repository.join("styles")) {
        let styles = entries.flatten().map(|entry| entry.path());
        files.extend(styles.filter(|path| path.extension().is_some_and(|e| e == "star")));
    }
    files.sort();
    let mut lines = String::new();
    for path in files {
        let text = std::fs::read_to_string(&path).map_err(crate::store::with_path(&path))?;
        for (n, line) in text.lines().enumerate() {
            let code = line.trim_start();
            // `arch_names` is where a recipe says how its project names each target.
            if code.starts_with('#') || code.starts_with("arch_names") {
                continue;
            }
            if let Some(name) = NAMES.iter().find(|name| names_word(code, name)) {
                let file = path.strip_prefix(repository).unwrap_or(&path);
                lines.push_str(&format!(
                    "{}:{}: names the target `{name}`; write {{arch}} or {{triple}} in a source, \
                     $WRATH_ARCH, $WRATH_TRIPLE or $WRATH_LOADER in a script\n",
                    file.display(),
                    n + 1
                ));
            }
        }
    }
    Ok(lines)
}

/// Whether `code` has `name` as a word of its own, so `x64` in `0x64` doesn't count.
fn names_word(code: &str, name: &str) -> bool {
    code.match_indices(name).any(|(at, _)| {
        let before = code[..at].chars().next_back();
        let after = code[at + name.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// Each system recipe's listed dependencies that another dependency it lists already brings,
/// one line per recipe, after building what isn't built yet.
pub fn redundant(repository: &Path, work: &Path, only: &[&str]) -> Result<String, String> {
    let lab = Lab::new(work);
    let wanted = (!only.is_empty()).then_some(only);
    let (mut built, _, mut packages) = pilot::build_some(&lab, repository, wanted)?;
    if let Some(wanted) = wanted {
        packages.retain(|package| wanted.contains(&package.id.as_str()));
    }
    let mut lines = String::new();
    for (id, deps) in chain::redundant_deps(&lab, &mut built, &packages)? {
        let listed: Vec<String> = deps
            .iter()
            .map(|(dep, by)| format!("{dep} (from {by})"))
            .collect();
        lines.push_str(&format!("{id}: {}\n", listed.join(", ")));
    }
    Ok(lines)
}

/// A system built from a model.
pub struct System {
    pub generation: String,
    /// The generation on the host.
    pub path: PathBuf,
    /// Packages the model names that have no recipe, so the system goes without them.
    pub skipped: Vec<String>,
    /// Units the model enables that no package the system has provides.
    pub unfound: Vec<String>,
    pub outcome: Outcome,
}

/// Lowers `model` with `packages` and realises it.
fn system_from(
    lab: &Lab,
    db: &mut Db,
    packages: &Packages,
    model: &Value,
    kept: &Kept,
) -> Result<System, String> {
    let Lowered {
        actions,
        skipped,
        unfound,
    } = lower(lab, model, kept, packages)?;
    let outcome = realise(lab, db, actions)?;
    Ok(System {
        path: lab.on_host(&StorePath::from_hash(&outcome.generation)),
        generation: outcome.generation.clone(),
        skipped,
        unfound,
        outcome,
    })
}

/// The recipes the model's `builds` start from, such as `locales` for a system's own locales,
/// which nothing the system runs may reach otherwise.
fn build_bases(model: &Value) -> Vec<String> {
    model["builds"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, build)| build["package"].as_str().map(str::to_owned))
        .collect()
}

/// The recipes [`build_bases`] names, with what they reach, for working out the builds made
/// from them without building the recipes themselves.
fn build_base_recipes(
    lab: &Lab,
    repository: &Path,
    model: &Value,
) -> Result<Vec<chain::Package>, String> {
    let bases = build_bases(model);
    if bases.is_empty() {
        return Ok(Vec::new());
    }
    chain::load_reached(
        lab,
        repository,
        &names_of(&recipe_files(repository)?),
        &names_of(&bases),
    )
}

/// The recipes the model's `builds` make: each a recipe under the build's name, with the
/// options and patches it gives, styled again when its recipe has a style.
fn variants(
    lab: &Lab,
    repository: &Path,
    recipes: &[chain::Package],
    model: &Value,
) -> Result<Vec<chain::Package>, String> {
    model["builds"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, build)| {
            let base = str_field(&build["package"], "build's package")?;
            let recipe = recipes
                .iter()
                .find(|recipe| recipe.id == base)
                .ok_or_else(|| {
                    format!("there's no recipe for `{base}`, which the build `{name}` starts from")
                })?;
            let options = build["options"].as_object().cloned().unwrap_or_default();
            let patches: Vec<String> = build["patches"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            recipe
                .variant(name, &options, &patches)
                .and_then(|variant| chain::restyled(lab, repository, variant))
                .map_err(|e| format!("`builds.{name}.options`: {e}"))
        })
        .collect()
}

/// The key each package of the toolchain, the system recipes `model` reaches and the builds it asks
/// for has before anything is built: what [`PACKAGE_KEYS`] records, from today's recipes.
pub fn package_keys(
    repository: &Path,
    work: &Path,
    model: &Value,
) -> Result<BTreeMap<String, String>, String> {
    let lab = Lab::new(work);
    let mut wanted = chain::TOOLCHAIN.map(str::to_owned).to_vec();
    wanted.extend(system_packages(model));
    wanted.extend(build_bases(model));
    let mut packages = chain::load_reached(
        &lab,
        repository,
        &names_of(&recipe_files(repository)?),
        &names_of(&wanted),
    )?;
    let builds = variants(&lab, repository, &packages, model)?;
    packages.extend(builds);
    chain::keys_of(&lab, repository, packages)
}

/// What of the system `model` describes is found by its recipe (0041), and whether that's all it
/// uses. A package found that something not found builds with isn't kept: that build reads it,
/// so it's worked out as before, with what it's built with.
fn found_by_recipe(lab: &Lab, repository: &Path, model: &Value) -> Result<(Built, bool), String> {
    let wanted = system_packages(model);
    // Keys are worked out from the toolchain up, though only what the system uses is looked up.
    let mut roots = chain::TOOLCHAIN.map(str::to_owned).to_vec();
    roots.extend(wanted.iter().cloned());
    roots.extend(build_bases(model));
    let mut packages = chain::load_reached(
        lab,
        repository,
        &names_of(&recipe_files(repository)?),
        &names_of(&roots),
    )?;
    let builds = variants(lab, repository, &packages, model)?;
    let mut ids: BTreeSet<String> = wanted.into_iter().collect();
    ids.extend(builds.iter().map(|build| build.id.clone()));
    packages.extend(builds);
    let keys = chain::keys_of(lab, repository, packages.clone())?;
    // As a build has it: what's built at the first stage refers to the seed.
    let seed = crate::seed::path(lab)?;
    let mut tried = chain::built_from_seed(&seed);
    for package in &packages {
        chain::found_by_recipe(lab, package, &keys[&package.id], &mut tried)?;
    }
    let missed: Vec<&str> = ids
        .iter()
        .filter(|id| !tried.paths.contains_key(*id))
        .map(String::as_str)
        .collect();
    if missed.is_empty() {
        // What it's built with is found where it can be too, as the system shares whatever of it
        // runs, such as gcc's libraries; a tool that only built it isn't in the store, so nothing
        // could share it.
        return Ok((tried, true));
    }
    let worked_out = built_with(&packages, &missed)?;
    let mut built = chain::built_from_seed(&seed);
    for package in packages.iter().filter(|p| !worked_out.contains(&p.id)) {
        chain::found_by_recipe(lab, package, &keys[&package.id], &mut built)?;
    }
    Ok((built, false))
}

/// `missed` and everything among `packages` they're built with, all the way down.
fn built_with(packages: &[chain::Package], missed: &[&str]) -> Result<BTreeSet<String>, String> {
    let by_id: BTreeMap<&str, &chain::Package> =
        packages.iter().map(|p| (p.id.as_str(), p)).collect();
    let mut reached = BTreeSet::new();
    let mut next: Vec<String> = missed.iter().map(|id| (*id).to_owned()).collect();
    while let Some(id) = next.pop() {
        if !reached.insert(id.clone()) {
            continue;
        }
        if let Some(package) = by_id.get(id.as_str()) {
            next.extend(package.dep_ids()?);
        }
    }
    Ok(reached)
}

/// Builds what the model's `builds` ask for: each a recipe built under the build's name with the
/// options it gives, which the recipe's script reads as `OPTION_<name>`, and its patches.
fn build_variants(
    lab: &Lab,
    repository: &Path,
    recipes: &[chain::Package],
    seed: &StorePath,
    built: &mut Built,
    model: &Value,
) -> Result<(), String> {
    let mut sources = chain::Sources::load(repository)?;
    for variant in variants(lab, repository, recipes, model)? {
        if !built.paths.contains_key(&variant.id) {
            pilot::build_and_finish(lab, repository, &variant, seed, &mut sources, built)?;
        }
    }
    Ok(())
}

/// The packages a system built from `model` uses: the base system's and the initrd's, its
/// kernel, microcode and firmware, the packages it and its users name, and those its builds start
/// from. A system build builds them and what they depend on, and no other recipe.
fn system_packages(model: &Value) -> Vec<String> {
    let strings = |value: &Value| -> Vec<String> {
        let items = value.as_array().into_iter().flatten();
        items.filter_map(Value::as_str).map(str::to_owned).collect()
    };
    let initrd = initrd_packages(&early::disks(&model["base"]["storage"]));
    let mut ids: BTreeSet<String> = MECHANISM_PACKAGES
        .iter()
        .chain(&initrd)
        .chain(&LINKED)
        .chain(&["dbus"])
        .map(|id| (*id).to_owned())
        .collect();
    ids.extend(strings(&model["packages"]));
    ids.extend(strings(&model["service_packages"]));
    for home in model["homes"]
        .as_object()
        .into_iter()
        .flat_map(|homes| homes.values())
    {
        ids.extend(strings(&home["packages"]));
    }
    for build in model["builds"]
        .as_object()
        .into_iter()
        .flat_map(|builds| builds.values())
    {
        ids.extend(build["package"].as_str().map(str::to_owned));
    }
    let kernel = &model["base"]["kernel"];
    ids.extend(kernel["package"].as_str().map(str::to_owned));
    ids.extend(kernel["microcode"]["package"].as_str().map(str::to_owned));
    ids.extend(strings(&kernel["firmware"]));
    let foreign = strings(&model["base"]["foreign"]["libraries"]);
    if !foreign.is_empty() {
        ids.insert("glibc".to_owned());
        ids.extend(foreign);
    }
    ids.into_iter().collect()
}

/// Builds the system `model` describes from the recipes in `repository`, which holds `recipes/`,
/// the pins and the `work` directory with the store, building packages it doesn't have yet.
pub fn system(
    repository: &Path,
    work: &Path,
    model: &Value,
    kept: &Kept,
) -> Result<System, String> {
    let lab = Lab::new(work);
    let built = match found_by_recipe(&lab, repository, model)? {
        (built, true) => built,
        (found, false) => {
            let wanted = system_packages(model);
            let (mut built, seed, mut recipes) =
                pilot::build_onto(&lab, repository, Some(&names_of(&wanted)), found)?;
            recipes.extend(build_base_recipes(&lab, repository, model)?);
            build_variants(&lab, repository, &recipes, &seed, &mut built, model)?;
            built
        }
    };
    let mut db = Db::open(&db_path(&lab))?;
    record_packages(&mut db, &built)?;
    let system = system_from(&lab, &mut db, &known_packages(&built), model, kept)?;
    // What it runs is in the store, fetched where a package was found but only traced.
    chain::runtime_closure(&lab, &StorePath::from_hash(&system.generation))?;
    if let Some(name) = system.path.file_name() {
        let keys = built
            .records
            .values()
            .map(|record| (record.unresolved.as_str(), record.resolved.as_str()));
        let system_path = StorePath::from_hash(&name.to_string_lossy());
        crate::records::keep(&lab, &system_path, keys, &system.outcome.outputs)?;
    }
    Ok(system)
}

/// What building a system would take: the packages that would be built, and for each cache what
/// would be fetched from it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Takes {
    /// What would be built, and why.
    pub build: BTreeMap<String, pilot::Why>,
    /// Whether those builds need the seed prepared first, or can't have it here.
    pub seed: Seed,
    /// How many store paths, and how many bytes, by the cache they come from.
    pub fetch: BTreeMap<String, (usize, u64)>,
}

/// The seed, as what would be built needs it.
#[derive(Debug, Default, PartialEq, Eq)]
pub enum Seed {
    /// Ready, or nothing is built.
    #[default]
    Ready,
    /// Prepared before the first build.
    Prepare,
    /// Neither here nor downloadable, so nothing can be built.
    Missing,
}

/// What building the system `model` describes would take, building nothing. Caches are asked
/// for traces, and nothing else.
pub fn system_takes(repository: &Path, work: &Path, model: &Value) -> Result<Takes, String> {
    let lab = Lab::new(work);
    // Found by recipe, nothing is built; what runs may still come from a cache.
    let (found, complete) = found_by_recipe(&lab, repository, model)?;
    if complete {
        let mut paths = BTreeSet::new();
        for id in system_packages(model)
            .iter()
            .filter(|id| found.paths.contains_key(*id))
        {
            for path in found.run_outputs(id).into_iter().filter(|p| there(&lab, p)) {
                paths.extend(chain::traced_closure(&lab, &path)?);
            }
        }
        return Ok(Takes {
            fetch: by_cache(&lab, &paths),
            ..Takes::default()
        });
    }
    let wanted = system_packages(model);
    let mut dry = pilot::Dry::onto(&lab, repository, &names_of(&wanted), found)?;
    let mut ids: BTreeSet<String> = wanted.iter().cloned().collect();
    let mut loaded = dry.loaded.clone();
    loaded.extend(build_base_recipes(&lab, repository, model)?);
    for variant in variants(&lab, repository, &loaded, model)? {
        if !dry.built.paths.contains_key(&variant.id) {
            dry.add(&lab, repository, &variant)?;
        }
        ids.insert(variant.id.clone());
    }
    // What runs, and what the builds of what's missing build with.
    let present = ids
        .iter()
        .filter(|id| dry.built.paths.contains_key(*id) && !dry.missing.contains_key(*id));
    let mut paths = dry.build_closure.clone();
    for id in present {
        for path in dry
            .built
            .run_outputs(id)
            .into_iter()
            .filter(|path| there(&lab, path))
        {
            paths.extend(chain::traced_closure(&lab, &path)?);
        }
    }
    let seed = if dry.missing.is_empty() || crate::seed::ready(&lab)? {
        Seed::Ready
    } else if crate::seed::available(&lab)? {
        Seed::Prepare
    } else {
        Seed::Missing
    };
    Ok(Takes {
        fetch: by_cache(&lab, &paths),
        build: dry.missing,
        seed,
    })
}

/// Whether `path` is in the store or traced by a cache. The seed, say, is in neither when nothing
/// has built yet, and nothing it holds is fetched.
fn there(lab: &Lab, path: &StorePath) -> bool {
    lab.on_host(path).exists() || crate::cache::traced(lab, path).is_some()
}

/// How many of `paths`, and how many bytes of them, would be fetched from each cache.
fn by_cache<'a>(
    lab: &Lab,
    paths: impl IntoIterator<Item = &'a StorePath>,
) -> BTreeMap<String, (usize, u64)> {
    let mut fetch: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    for (cache, size) in paths
        .into_iter()
        .filter(|path| !crate::cache::holds_nothing(lab, path))
        .filter_map(|path| crate::cache::traced(lab, path))
    {
        let (count, bytes) = fetch.entry(cache).or_default();
        *count += 1;
        *bytes += size;
    }
    fetch
}

/// Where one package would come from, as `wrath cache check` says it.
#[derive(Debug, PartialEq, Eq)]
pub enum Found {
    /// In the store already.
    Here,
    /// From caches: how many paths and bytes from each.
    Fetched(BTreeMap<String, (usize, u64)>),
    /// Built: the packages that lead to one no cache has, starting with this one's dependency,
    /// empty when no cache has this one itself.
    Built(Vec<String>),
}

/// What it takes to have package `id`, with its key when one can be worked out: a package built
/// with one that would be built has none until that's built.
pub fn package_check(
    repository: &Path,
    work: &Path,
    id: &str,
) -> Result<(Option<String>, Found), String> {
    let lab = Lab::new(work);
    let dry = pilot::Dry::of(&lab, repository, &[id])?;
    let key = dry.built.temporary.get(id).map(|key| key.hash().to_owned());
    let mut chain = Vec::new();
    let mut at = id;
    while let Some(pilot::Why::BuiltWith(dep)) = dry.missing.get(at) {
        chain.push(dep.clone());
        at = dep;
    }
    if dry.missing.contains_key(id) {
        return Ok((key, Found::Built(chain)));
    }
    let mut paths = BTreeSet::new();
    for output in dry.built.run_outputs(id) {
        if there(&lab, &output) {
            paths.extend(chain::traced_closure(&lab, &output)?);
        }
    }
    let fetched = by_cache(&lab, &paths);
    let found = if fetched.is_empty() {
        Found::Here
    } else {
        Found::Fetched(fetched)
    };
    Ok((key, found))
}

/// The key every recipe in `repository` has before anything is built, by id.
pub fn all_keys(repository: &Path, work: &Path) -> Result<BTreeMap<String, String>, String> {
    let lab = Lab::new(work);
    let mut packages = Vec::new();
    for file in recipe_files(repository)? {
        packages.extend(chain::load_styled(&lab, repository, &file)?);
    }
    chain::keys_of(&lab, repository, packages)
}

/// Styles every recipe whose lock doesn't record what its style makes now, recording it, and
/// returns their ids. With the locks read only, the first such recipe is an error.
pub fn record_styles(repository: &Path, work: &Path) -> Result<Vec<String>, String> {
    let lab = Lab::new(work);
    let mut styled = Vec::new();
    for file in recipe_files(repository)? {
        styled.extend(
            chain::load_styled(&lab, repository, &file)?
                .into_iter()
                .filter(chain::Package::restyled_now)
                .map(|package| package.id),
        );
    }
    Ok(styled)
}

/// The id of every recipe in `repository`, the names packages can have, read without building
/// anything.
pub fn recipe_ids(repository: &Path) -> Result<BTreeSet<String>, String> {
    let mut ids = BTreeSet::new();
    for file in recipe_files(repository)? {
        ids.extend(
            chain::load(repository, &file)?
                .into_iter()
                .map(|package| package.id),
        );
    }
    Ok(ids)
}

/// Builds the packages `names` from the recipes in `repository`, as [`system`] would, and returns
/// where each one is in the store.
pub fn packages(repository: &Path, work: &Path, names: &[&str]) -> Result<Vec<StorePath>, String> {
    let lab = Lab::new(work);
    pilot::unknown(names, &recipe_ids(repository)?)?;
    let (built, ..) = pilot::build_some(&lab, repository, Some(names))?;
    names
        .iter()
        .map(|name| {
            built
                .paths
                .get(*name)
                .cloned()
                .ok_or_else(|| format!("there's no recipe for `{name}`"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_isnt_found_is_worked_out_with_what_it_builds_with_and_nothing_above() {
        let package = |id: &str, deps: &str| {
            chain::recipe_from_str(&format!(
                "[[package]]\nid = \"{id}\"\nscope = \"runtime\"\ntoolchain = \"seed\"\n\
                 script = \"\"\ndeps = [{deps}]\n"
            ))
        };
        let packages = [
            package("zlib", ""),
            package("oniguruma", ""),
            package("jq", "\"oniguruma\""),
            package("curl", "\"zlib\""),
            package("git", "\"curl\", \"zlib\""),
        ];
        let ids = |set: BTreeSet<String>| set.into_iter().collect::<Vec<_>>();

        assert_eq!(
            ids(built_with(&packages, &["curl"]).unwrap()),
            ["curl", "zlib"]
        );
        assert_eq!(
            ids(built_with(&packages, &["jq", "git"]).unwrap()),
            ["curl", "git", "jq", "oniguruma", "zlib"]
        );
        assert_eq!(
            ids(built_with(&packages, &[]).unwrap()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn recipes_that_write_a_targets_name_are_pointed_out() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("recipes")).unwrap();
        fs::write(
            dir.path().join("recipes/a.toml"),
            "# x86_64 in a comment is fine\nsources = [{ url = \"zig-{arch}-{os}.tar.xz\" }]\n\
             script = 'make x86_64_defconfig'\n",
        )
        .unwrap();
        let found = targets_written(dir.path()).unwrap();
        assert!(
            found.starts_with("recipes/a.toml:3: names the target `x86_64`"),
            "{found}"
        );
        assert_eq!(found.lines().count(), 1, "{found}");
    }

    #[test]
    fn a_recipe_file_goes_under_its_name_and_declares_ids_no_other_does() {
        let dir = tempfile::tempdir().unwrap();
        let recipe = |id: &str| format!("[[package]]\nid = \"{id}\"\nscope = \"runtime\"\n");
        for (file, text) in [
            ("jq/jq.toml", recipe("jq")),
            ("gt/gtk4.toml", recipe("gtk4") + &recipe("gtk4-gir")),
            ("gt/tools.toml", recipe("gtk4-gir")),
            ("ripgrep.toml", recipe("ripgrep")),
        ] {
            let path = dir.path().join("recipes").join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let found = misplaced_recipes(dir.path()).unwrap();
        let lines: Vec<&str> = found.lines().collect();
        assert_eq!(
            lines,
            [
                "recipes/gt/tools.toml: named after none of its packages; name it after the main \
                 one",
                "recipes/gt/tools.toml: goes in recipes/to/, under the first letters of its name",
                "`gtk4-gir` is declared in recipes/gt/gtk4.toml and recipes/gt/tools.toml",
                "recipes/ripgrep.toml: goes in recipes/ri/, under the first letters of its name",
            ]
        );
    }

    #[test]
    fn a_system_builds_what_it_uses_and_nothing_else() {
        let model = json!({
            "packages": ["jq", "patched"],
            "service_packages": ["polkit"],
            "homes": {"john": {"packages": ["ripgrep"]}},
            "builds": {"patched": {"package": "gnu-hello"}},
            "base": {"kernel": {
                "package": "linux",
                "microcode": {"package": "microcode-amd"},
                "firmware": ["linux-firmware"],
            }},
        });
        let wanted = system_packages(&model);
        for id in [
            "jq",
            "polkit",
            "ripgrep",
            "gnu-hello",
            "linux",
            "microcode-amd",
            "linux-firmware",
            "systemd",
            "dbus",
            "bash",
            "cryptsetup",
        ] {
            assert!(wanted.iter().any(|w| w == id), "{id}");
        }
        assert!(!wanted.iter().any(|w| w == "mesa"));
    }

    fn fake(lab: &Lab, name: &str, programs: &[&str]) -> PackageRef {
        let path = StorePath::derive("test", &[name.as_bytes()]);
        let bin = lab.on_host(&path).join("bin");
        fs::create_dir_all(&bin).unwrap();
        for program in programs {
            fs::write(bin.join(program), "").unwrap();
        }
        PackageRef {
            outputs: vec![path.clone()],
            path,
            unresolved: format!("unresolved-{name}"),
            indexes: Vec::new(),
        }
    }

    #[test]
    fn a_time_zone_is_one_tzdata_names() {
        assert_eq!(
            zone_file("Europe/Stockholm").unwrap(),
            "/run/system/share/zoneinfo/Europe/Stockholm"
        );
        assert!(zone_file("UTC").is_ok());
        assert!(zone_file("Etc/GMT+1").is_ok());
        for wrong in ["", "../../etc/shadow", "/Europe/Stockholm", "Europe//Oslo"] {
            assert!(zone_file(wrong).is_err(), "{wrong}");
        }
    }

    fn setup() -> (tempfile::TempDir, Lab, Packages) {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path().to_str().unwrap());
        fs::create_dir_all(dir.path().join("store")).unwrap();
        let mut packages = Packages::new();
        for id in MECHANISM_PACKAGES
            .iter()
            .chain(INITRD_PACKAGES.iter())
            .chain(["web", "linux"].iter())
        {
            let programs: &[&str] = match *id {
                "web" => &["web"],
                "wrath" => &["wrath"],
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
        assert_eq!(changed, ["etc/wrath/model-hash", "etc/wrath/model.json"]);
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
    fn a_system_is_its_generation_and_what_it_skipped() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let mut model = model(80, false);
        model["packages"] = json!(["web", "missing"]);

        let built = system_from(&lab, &mut db, &packages, &model, &Kept::plain(&model)).unwrap();
        assert_eq!(built.skipped, ["missing"]);
        assert_eq!(
            built.path,
            lab.on_host(&StorePath::from_hash(&built.generation))
        );
        assert!(built.path.join("etc/web.conf").exists());

        let again = system_from(&lab, &mut db, &packages, &model, &Kept::plain(&model)).unwrap();
        assert_eq!(again.generation, built.generation);
        assert_eq!(again.outcome.ran, 0);
    }

    #[test]
    fn enabled_units_go_into_the_targets_named_or_those_they_install_into() {
        let (_dir, lab, packages) = setup();
        let units = lab
            .on_host(&packages["systemd"].path)
            .join("lib/systemd/system");
        fs::create_dir_all(&units).unwrap();
        let install =
            "[Unit]\nWantedBy=nothing.target\n[Install]\nWantedBy=a.target ../../x b.target\n";
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
    fn a_program_given_by_its_path_is_left_as_it_is() {
        let (_dir, lab, packages) = setup();
        let web = &packages["web"].path;
        let sections = json!([{"name": "Service", "entries": [
            ["ExecStartPre", "/bin/sh -c true"],
            ["ExecStart", "/run/system/bin/web"],
        ]}]);
        let rendered = render_unit(&sections, &[(lab.on_host(web), web.inside())]).unwrap();
        assert_eq!(
            rendered,
            "[Service]\nExecStartPre=/bin/sh -c true\nExecStart=/run/system/bin/web\n\n"
        );
    }

    #[test]
    fn a_program_no_package_has_is_an_error_rather_than_a_unit_that_cant_start() {
        let (_dir, lab, packages) = setup();
        let web = &packages["web"].path;
        let sections = json!([{"name": "Service", "entries": [
            ["ExecStart", "systemd-repart --dry-run=no"],
        ]}]);
        let error = render_unit(&sections, &[(lab.on_host(web), web.inside())]).unwrap_err();
        assert_eq!(
            error,
            "`ExecStart` runs `systemd-repart`, which none of the system's packages has"
        );
    }

    #[test]
    fn a_unit_from_a_package_every_system_has_can_be_enabled() {
        let (_dir, lab, packages) = setup();
        let units = lab
            .on_host(&packages["wrath"].path)
            .join("lib/systemd/system");
        fs::create_dir_all(&units).unwrap();
        fs::write(units.join("wrath-agent.service"), "[Service]\n").unwrap();
        let mut model = model(80, false);
        model["enabled"] = json!({"wrath-agent.service": ["multi-user.target"]});
        let lowered = lower(&lab, &model, &Kept::plain(&model), &packages).unwrap();
        assert!(lowered.unfound.is_empty(), "{:?}", lowered.unfound);
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realise(&lab, &mut db, lowered.actions).unwrap();
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let wants = root.join("etc/systemd/system/multi-user.target.wants/wrath-agent.service");
        assert!(fs::symlink_metadata(wants).is_ok());
        // systemd finds a wanted unit by its name, on its own search path.
        let unit = fs::read_link(root.join("etc/systemd/system/wrath-agent.service")).unwrap();
        let wrath = packages["wrath"].path.inside();
        assert_eq!(
            unit,
            Path::new(&format!("{wrath}/lib/systemd/system/wrath-agent.service"))
        );
    }

    #[test]
    fn the_initrd_unlocks_and_mounts_what_the_model_describes_then_readies_the_root() {
        let disks = early::Disks {
            crypttab: "main0-system /dev/disk/by-partlabel/main0-system none luks\n".into(),
            initrd_fstab: "/dev/mapper/main0-system /sysroot/wrath btrfs subvol=@wrath 0 0\n"
                .into(),
            modules: vec!["dm_crypt".into(), "btrfs".into()],
            ..early::Disks::default()
        };
        let files: BTreeMap<String, String> =
            initrd_files(&disks, "w-01", "options nbd nbds_max=2\n")
                .into_iter()
                .map(|(path, _, text)| (path, text))
                .collect();
        assert_eq!(files["etc/crypttab"], disks.crypttab);
        assert_eq!(files["etc/fstab"], disks.initrd_fstab);
        assert_eq!(files["etc/modules-load.d/wrath.conf"], "dm_crypt\nbtrfs\n");
        assert_eq!(files["etc/hostname"], "w-01\n");
        assert!(files["etc/initrd-release"].contains("ID=wrath\n"));
        let root = &files["etc/systemd/system/wrath-root.service"];
        assert!(root.contains("ExecStart=/etc/wrath-root\n"), "{root}");
        assert!(files["etc/wrath-root"].contains("os-release"));
        assert!(!files.contains_key("etc/ssh/sshd_config"));
    }

    #[test]
    fn an_initrd_with_ssh_lets_its_keys_in_with_a_host_key_from_a_credential() {
        let disks = early::Disks {
            ssh: Some(early::Ssh {
                port: 2022,
                authorized_keys: vec!["ssh-ed25519 AAAA me".into()],
            }),
            ..early::Disks::default()
        };
        let files: BTreeMap<String, String> = initrd_files(&disks, "", "")
            .into_iter()
            .map(|(path, _, text)| (path, text))
            .collect();
        let config = &files["etc/ssh/sshd_config"];
        assert!(
            config.contains("Port 2022\n") && config.contains("UsePAM no\n"),
            "{config}"
        );
        assert_eq!(
            files["etc/ssh/authorized_keys.d/root"],
            "ssh-ed25519 AAAA me\n"
        );
        let unit = &files["etc/systemd/system/wrath-initrd-ssh.service"];
        assert!(
            unit.contains("LoadCredentialEncrypted=wrath.initrd-ssh-key\n")
                && unit.contains("-h %d/wrath.initrd-ssh-key\n"),
            "{unit}"
        );
        assert!(files["etc/systemd/network/10-wired.network"].contains("DHCP=yes"));
        assert!(files["etc/passwd"].contains("\nsshd:x:74:74:"));
        assert!(files["etc/group"].contains("\nsystemd-network:x:152:\n"));
        assert_eq!(initrd_packages(&disks).last(), Some(&"openssh"));
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
    fn every_action_is_keyed_by_the_code_that_lowers_it_too() {
        let (_dir, lab, packages) = setup();
        let model = model(80, false);
        let lowered = lower(&lab, &model, &Kept::plain(&model), &packages).unwrap();
        for action in &lowered.actions {
            for record in [&action.resolved, &action.unresolved] {
                assert_eq!(record["lowering"], json!(lowering()), "{}", action.origin);
            }
        }
    }

    #[test]
    fn a_system_carries_its_encrypted_secrets_and_what_the_machine_fills_in() {
        let (_dir, lab, mut packages) = setup();
        packages.insert("age".to_owned(), fake(&lab, "age", &["age"]));
        let mut model = model(80, false);
        model["base"]["system"]["hostname"] = json!("");
        model["base"]["secrets"] = json!({"identities": ["/etc/ssh/ssh_host_ed25519_key"]});
        model["secrets"] = json!({"token": {"hash": "h", "owner": "web", "mode": 0o440}});
        model["templates"] = json!({
            "/etc/hostname": {"parts": [{"machine": "machine.name"}, {"text": "\n"}],
                "owner": "root", "mode": 0o644},
            "/etc/web/env": {"parts": [{"text": "T="}, {"secret": "token"}],
                "owner": "web", "group": "web", "mode": 0o400},
        });
        let mut kept = Kept::plain(&model);
        kept.secrets
            .insert("token".to_owned(), b"age-encrypted".to_vec());
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let lowered = lower(&lab, &model, &kept, &packages).unwrap();
        let outcome = realise(&lab, &mut db, lowered.actions).unwrap();
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let read = |path: &str| fs::read_to_string(root.join(path)).unwrap();

        assert_eq!(read("secrets/token.age"), "age-encrypted");
        let spec: Value = serde_json::from_str(&read("templates.json")).unwrap();
        assert_eq!(
            spec["etc_web_env"],
            json!([{"text": "T="}, {"secret": "token"}])
        );
        assert_eq!(
            fs::read_link(root.join("etc/web/env")).unwrap(),
            Path::new("/run/wrath/templates/etc_web_env")
        );
        assert_eq!(
            fs::read_link(root.join("etc/hostname")).unwrap(),
            Path::new("/run/wrath/templates/etc_hostname")
        );
        let activation: Value = serde_json::from_str(&read("activation.json")).unwrap();
        assert_eq!(
            activation["secrets"]["token"],
            json!({"source": "secrets/token.age", "owner": "web", "mode": "0440", "link": ""})
        );
        let age = packages["age"].path.inside();
        assert_eq!(activation["age"], json!(format!("{age}/bin/age")));
        assert_eq!(
            activation["identities"],
            json!(["/etc/ssh/ssh_host_ed25519_key"])
        );
        assert_eq!(activation["hostname_filled"], json!(true));
        assert_eq!(activation["templates_spec"], json!("templates.json"));
        assert!(
            activation["templates"]
                .as_array()
                .unwrap()
                .contains(&json!({"name": "etc_web_env", "owner": "web:web", "mode": "0400"}))
        );
    }

    #[test]
    fn glibc_finds_the_nss_modules_packages_ship_in_the_systems_lib_nss() {
        let (_dir, lab, packages) = setup();
        let nss = lab.on_host(&packages["systemd"].path).join("lib/nss");
        fs::create_dir_all(&nss).unwrap();
        fs::write(nss.join("libnss_systemd.so.2"), "").unwrap();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &model(80, false));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let systemd = packages["systemd"].path.inside();
        assert_eq!(
            fs::read_link(root.join("lib/nss/libnss_systemd.so.2")).unwrap(),
            Path::new(&format!("{systemd}/lib/nss/libnss_systemd.so.2"))
        );
    }

    #[test]
    fn pam_finds_the_modules_packages_ship_in_the_systems_lib_security() {
        let (_dir, lab, packages) = setup();
        let security = lab.on_host(&packages["systemd"].path).join("lib/security");
        fs::create_dir_all(&security).unwrap();
        fs::write(security.join("pam_systemd.so"), "").unwrap();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &model(80, false));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let systemd = packages["systemd"].path.inside();
        assert_eq!(
            fs::read_link(root.join("lib/security/pam_systemd.so")).unwrap(),
            Path::new(&format!("{systemd}/lib/security/pam_systemd.so"))
        );
    }

    fn with_disks(mut model: Value) -> Value {
        model["base"]["storage"] = json!({
            "partitions": [{"encrypt": true, "label": "main0-system"}],
            "mounts": [
                {"device": "none", "format": "tmpfs", "options": ["mode=755"], "path": "/"},
                {"device": "/dev/mapper/main0-system", "format": "btrfs",
                 "options": ["subvol=@wrath"], "path": "/wrath"},
            ],
            "unlock": {},
        });
        model
    }

    #[test]
    fn a_system_on_disks_mounts_and_unlocks_them_and_finds_its_root_by_its_fstab() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &with_disks(model(80, false)));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let read = |path: &str| fs::read_to_string(root.join(path)).unwrap();
        assert!(read("etc/fstab").contains("/dev/mapper/main0-system /wrath btrfs subvol=@wrath"));
        assert!(
            read("etc/crypttab").starts_with("main0-system /dev/disk/by-partlabel/main0-system")
        );
        assert!(
            !read("kernel-params").contains("root="),
            "{}",
            read("kernel-params")
        );
    }

    #[test]
    fn a_system_without_disks_has_no_fstab_and_finds_its_root_by_label() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &model(80, false));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        assert!(!root.join("etc/fstab").exists());
        assert!(!root.join("etc/crypttab").exists());
    }

    #[test]
    fn the_initrd_loads_only_modules_the_kernel_has() {
        let dir = tempfile::tempdir().unwrap();
        let version = dir.path().join("6.16.1");
        fs::create_dir_all(&version).unwrap();
        fs::write(
            version.join("modules.builtin"),
            "kernel/fs/btrfs/btrfs.ko\n",
        )
        .unwrap();
        fs::write(
            version.join("modules.dep"),
            "kernel/drivers/md/dm-crypt.ko.zst: kernel/drivers/md/dm-mod.ko.zst\n",
        )
        .unwrap();
        let wanted = ["btrfs", "dm_crypt", "tmpfs", "af_alg"].map(String::from);
        assert_eq!(kernel_has(dir.path(), &wanted), ["btrfs", "dm_crypt"]);
    }

    #[test]
    fn the_foreign_loader_reads_its_cache_from_run_and_is_otherwise_glibc_s() {
        let loader = b"\x7fELF\0/etc/ld.so.cache\0/etc/ld.so.preload\0";
        let patched = foreign_loader(loader).unwrap();
        assert_eq!(patched, b"\x7fELF\0/run/ld.so.cache\0/etc/ld.so.preload\0");
        let error = foreign_loader(b"\x7fELF\0").unwrap_err();
        assert_eq!(error, "glibc's loader doesn't name `/etc/ld.so.cache` once");
    }

    #[test]
    fn the_initrd_keeps_drivers_that_read_firmware_from_loading_until_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let version = dir.path().join("7.3.4");
        let modules = [
            ("kernel/drivers/md/dm-mod.ko", ""),
            ("kernel/drivers/block/virtio_blk.ko", ""),
            ("kernel/drivers/block/nvme.ko", ""),
            ("kernel/net/wireless/cfg80211.ko", "firmware=regulatory.db"),
            (
                "kernel/drivers/net/wireless/mt7925e.ko",
                "firmware=mt7925.bin",
            ),
            ("kernel/drivers/scsi/qla2xxx.ko", "firmware=ql2500_fw.bin"),
        ];
        let mut dep = String::new();
        for (path, info) in modules {
            let file = version.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, format!("\x7fELF\0license=GPL\0{info}\0")).unwrap();
            dep.push_str(&format!("{path}:\n"));
        }
        fs::write(version.join("modules.dep"), dep).unwrap();
        // The root's disk can need a driver nobody named, which udev finds for its device.
        let wanted = ["dm_mod", "qla2xxx"].map(String::from);
        assert_eq!(
            initrd_deny_list(dir.path(), &wanted),
            "blacklist cfg80211\nblacklist mt7925e\n"
        );
    }

    #[test]
    fn the_initrd_keeps_drivers_whose_dependencies_read_firmware_from_loading_too() {
        let dir = tempfile::tempdir().unwrap();
        let version = dir.path().join("7.3.4");
        // btusb loads btmtk, which asks for the radio's firmware as btusb sets it up; blocking
        // btmtk alone doesn't stop it loading as btusb's dependency.
        let modules = [
            (
                "kernel/drivers/bluetooth/btmtk.ko",
                "firmware=mt7925.bin",
                "",
            ),
            (
                "kernel/drivers/bluetooth/btusb.ko",
                "",
                "kernel/drivers/bluetooth/btmtk.ko",
            ),
            (
                "kernel/drivers/scsi/qla2xxx.ko",
                "",
                "kernel/drivers/scsi/qla_fw.ko",
            ),
            (
                "kernel/drivers/scsi/qla_fw.ko",
                "firmware=ql2500_fw.bin",
                "",
            ),
            ("kernel/drivers/block/nvme.ko", "", ""),
        ];
        let mut dep = String::new();
        for (path, info, needs) in modules {
            let file = version.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, format!("\x7fELF\0license=GPL\0{info}\0")).unwrap();
            dep.push_str(&format!("{path}: {needs}\n"));
        }
        fs::write(version.join("modules.dep"), dep).unwrap();
        let wanted = ["qla2xxx"].map(String::from);
        assert_eq!(
            initrd_deny_list(dir.path(), &wanted),
            "blacklist btmtk\nblacklist btusb\n",
            "what a wanted module needs still loads"
        );
    }

    #[test]
    fn a_service_packages_units_rules_and_dbus_services_join_the_system() {
        let (_dir, lab, packages) = setup();
        let web = lab.on_host(&packages["web"].path);
        for (dir, file) in [
            ("lib/udev/rules.d", "95-web.rules"),
            ("share/p11-kit/modules", "web.module"),
            ("lib/systemd/system", "web-helper.service"),
        ] {
            fs::create_dir_all(web.join(dir)).unwrap();
            fs::write(web.join(dir).join(file), "").unwrap();
        }
        let mut model = model(80, false);
        model["service_packages"] = json!(["web"]);
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &model);
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let linked = |path: &str| fs::symlink_metadata(root.join(path)).is_ok();
        assert!(linked("etc/udev/rules.d/95-web.rules"));
        assert!(linked("etc/pkcs11/modules/web.module"));
        assert!(linked("etc/systemd/system/web-helper.service"));
        let conf = fs::read_to_string(root.join("etc/dbus-1/system.d/services.conf")).unwrap();
        let web = packages["web"].path.inside();
        assert!(
            conf.contains(&format!("{web}/share/dbus-1/system-services")),
            "{conf}"
        );
        let session = fs::read_to_string(root.join("etc/dbus-1/session.d/services.conf")).unwrap();
        assert!(
            session.contains(&format!("{web}/share/dbus-1/services")),
            "{session}"
        );
    }

    #[test]
    fn a_service_packages_user_units_join_the_system_and_enabled_ones_are_wanted() {
        let (_dir, lab, packages) = setup();
        let units = lab.on_host(&packages["web"].path).join("lib/systemd/user");
        fs::create_dir_all(&units).unwrap();
        fs::write(
            units.join("web-user.service"),
            "[Install]\nWantedBy=default.target\n",
        )
        .unwrap();
        fs::write(
            units.join("web-user.socket"),
            "[Install]\nWantedBy=sockets.target\n",
        )
        .unwrap();
        fs::write(units.join("web-session.service"), "[Unit]\n").unwrap();
        let mut model = model(80, false);
        model["service_packages"] = json!(["web"]);
        model["enabled_user"] =
            json!({"web-user.service": [], "web-user.socket": ["other.target"], "nope.socket": []});
        let lowered = lower(&lab, &model, &Kept::plain(&model), &packages).unwrap();
        assert_eq!(lowered.unfound, ["nope.socket"]);
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realise(&lab, &mut db, lowered.actions).unwrap();
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let web = packages["web"].path.inside();
        for (link, unit) in [
            ("web-session.service", "web-session.service"),
            ("web-user.service", "web-user.service"),
            ("default.target.wants/web-user.service", "web-user.service"),
            ("other.target.wants/web-user.socket", "web-user.socket"),
        ] {
            let target = fs::read_link(root.join("etc/systemd/user").join(link)).unwrap();
            assert_eq!(target, Path::new(&format!("{web}/lib/systemd/user/{unit}")));
        }
        assert!(!root.join("etc/systemd/user/sockets.target.wants").exists());
    }

    #[test]
    fn a_system_leaves_its_machine_id_to_the_machine() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &model(80, false));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        assert!(!root.join("etc/machine-id").exists());
    }

    #[test]
    fn a_systems_share_merges_its_packages_data_without_translations_or_manuals() {
        let (_dir, lab, packages) = setup();
        let web = lab.on_host(&packages["web"].path);
        for file in [
            "share/fonts/web/a.ttf",
            "share/xdg-desktop-portal/portals/web.portal",
            "share/locale/sv/LC_MESSAGES/web.mo",
            "share/man/man1/web.1",
        ] {
            fs::create_dir_all(web.join(file).parent().unwrap()).unwrap();
            fs::write(web.join(file), "").unwrap();
        }
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &model(80, false));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let inside = packages["web"].path.inside();
        for file in [
            "share/fonts/web/a.ttf",
            "share/xdg-desktop-portal/portals/web.portal",
        ] {
            let target = fs::read_link(root.join(file)).unwrap();
            assert_eq!(target, Path::new(&format!("{inside}/{file}")));
        }
        assert!(!root.join("share/locale").exists());
        assert!(!root.join("share/man").exists());
    }

    fn with_schemas(lab: &Lab, packages: &Packages, id: &str, names: &[&str]) {
        let dir = lab
            .on_host(&packages[id].path)
            .join("share/glib-2.0/schemas");
        fs::create_dir_all(&dir).unwrap();
        for name in names {
            fs::write(dir.join(name), id).unwrap();
        }
    }

    fn schemas_index(packages: &mut Packages, from: &str) {
        packages.get_mut("dbus").unwrap().indexes = vec![chain::Index {
            from: from.to_owned(),
            run: "bin/glib-compile-schemas share/glib-2.0/schemas".to_owned(),
        }];
    }

    #[test]
    fn an_index_reads_what_packages_share_under_its_from_the_first_package_winning() {
        let (_dir, lab, packages) = setup();
        with_schemas(&lab, &packages, "bash", &["a.gschema.xml", "b.gschema.xml"]);
        with_schemas(&lab, &packages, "web", &["b.gschema.xml", "c.gschema.xml"]);
        let inputs = index_inputs(&lab, &["bash", "web"], &packages, "share/glib-2.0/schemas");
        let from: Vec<(&str, &Path)> = inputs
            .iter()
            .map(|(name, file)| (name.as_str(), file.parent().unwrap()))
            .collect();
        let dir = |id: &str| {
            lab.on_host(&packages[id].path)
                .join("share/glib-2.0/schemas")
        };
        let (bash, web) = (dir("bash"), dir("web"));
        assert_eq!(
            from,
            [
                ("a.gschema.xml", bash.as_path()),
                ("b.gschema.xml", bash.as_path()),
                ("c.gschema.xml", web.as_path())
            ]
        );
    }

    #[test]
    fn an_index_runs_only_when_a_package_has_something_under_its_from() {
        let (_dir, lab, mut packages) = setup();
        schemas_index(&mut packages, "share/glib-2.0/schemas");
        assert!(
            index_actions("packages", &lab, &["bash", "web"], &packages)
                .unwrap()
                .is_empty()
        );
        with_schemas(&lab, &packages, "web", &["a.gschema.xml"]);
        let actions = index_actions("packages", &lab, &["bash", "web"], &packages).unwrap();
        assert_eq!(actions.len(), 1);
        let record = &actions[0].resolved;
        assert_eq!(record["kind"], "index");
        assert_eq!(record["order"], json!(["web"]));
        let deps: Vec<&String> = record["deps"].as_object().unwrap().keys().collect();
        assert_eq!(deps, ["dbus", "web"]);
    }

    #[test]
    fn an_index_of_something_packages_dont_share_is_an_error() {
        let (_dir, lab, mut packages) = setup();
        schemas_index(&mut packages, "lib/gio/modules");
        assert_eq!(
            index_actions("packages", &lab, &["web"], &packages)
                .err()
                .unwrap(),
            "`dbus` indexes `lib/gio/modules`, which isn't in what packages share"
        );
    }

    /// `libweb`, which no configuration names, ships data and a program, and `web` refers to it.
    fn with_libweb(lab: &Lab, packages: &mut Packages) -> PathBuf {
        let libweb = fake(lab, "libweb", &["libweb-tool"]);
        let root = lab.on_host(&libweb.path);
        fs::create_dir_all(root.join("share/libweb")).unwrap();
        fs::write(root.join("share/libweb/data"), "libweb").unwrap();
        let web = lab.on_host(&packages["web"].path).join("bin/web");
        fs::write(web, format!("{}/lib/libweb.so\n", libweb.path.inside())).unwrap();
        packages.insert("libweb".to_owned(), libweb);
        root
    }

    #[test]
    fn a_system_shares_the_data_of_what_its_packages_run_but_not_its_programs() {
        let (_dir, lab, mut packages) = setup();
        with_libweb(&lab, &mut packages);
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &model(80, false));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let target = fs::read_link(root.join("share/libweb/data")).unwrap();
        let inside = packages["libweb"].path.inside();
        assert_eq!(target, Path::new(&format!("{inside}/share/libweb/data")));
        assert!(fs::symlink_metadata(root.join("bin/libweb-tool")).is_err());
        assert!(fs::symlink_metadata(root.join("bin/web")).is_ok());
    }

    #[test]
    fn two_packages_may_share_the_same_file_but_not_two_different_ones() {
        for (contents, clash) in [("libweb", false), ("changed", true)] {
            let (_dir, lab, mut packages) = setup();
            with_libweb(&lab, &mut packages);
            let web = lab.on_host(&packages["web"].path).join("share/libweb");
            fs::create_dir_all(&web).unwrap();
            fs::write(web.join("data"), contents).unwrap();
            let model = model(80, false);
            let actions = lower(&lab, &model, &Kept::plain(&model), &packages)
                .unwrap()
                .actions;
            let mut db = Db::open(&db_path(&lab)).unwrap();
            match realise(&lab, &mut db, actions) {
                Ok(_) => assert!(!clash),
                Err(error) => {
                    assert!(clash);
                    let start = "`share/libweb/data` is in two packages with different contents";
                    assert!(error.starts_with(start), "{error}");
                }
            }
        }
    }

    #[test]
    fn data_a_packages_libraries_refer_to_is_shared_from_its_library_output() {
        let (_dir, lab, mut packages) = setup();
        let lib = StorePath::derive("test", &[b"web-lib"]);
        let presets = lab.on_host(&lib).join("share/web/presets");
        fs::create_dir_all(&presets).unwrap();
        fs::write(presets.join("hinting.conf"), "slight").unwrap();
        packages.get_mut("web").unwrap().outputs.push(lib.clone());
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &model(80, false));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let link = fs::read_link(root.join("share/web/presets/hinting.conf")).unwrap();
        assert_eq!(
            link,
            Path::new(&format!("{}/share/web/presets/hinting.conf", lib.inside()))
        );
    }

    #[test]
    fn an_output_a_machine_never_got_takes_no_part_in_what_it_shares() {
        let (_dir, lab, mut packages) = setup();
        // A main output a split left empty, which a cache never sends, beside the library one.
        let gone = StorePath::derive("test", &[b"web-out-gone"]);
        packages.get_mut("web").unwrap().outputs.insert(0, gone);
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &model(80, false));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        assert!(fs::symlink_metadata(root.join("bin/web")).is_ok());
    }

    #[test]
    fn programs_a_package_keeps_in_sbin_are_on_the_path_too() {
        let (_dir, lab, packages) = setup();
        let sbin = lab.on_host(&packages["util-linux"].path).join("sbin");
        fs::create_dir_all(&sbin).unwrap();
        fs::write(sbin.join("useradd"), "").unwrap();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &model(80, false));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        assert!(fs::symlink_metadata(root.join("bin/useradd")).is_ok());
    }

    /// The model with firmware from `web`, as a machine's hardware.
    fn with_hardware(mut model: Value) -> Value {
        model["base"]["kernel"] = json!({
            "initrd_modules": ["nbd"],
            "modprobe": "options nbd nbds_max=2\n",
            "firmware": ["web"],
        });
        model
    }

    #[test]
    fn firmware_joins_the_system() {
        let (_dir, lab, packages) = setup();
        let firmware = lab
            .on_host(&packages["web"].path)
            .join("lib/firmware/zd1211");
        fs::create_dir_all(&firmware).unwrap();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &with_hardware(model(80, false)));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let web = packages["web"].path.inside();
        assert_eq!(
            fs::read_link(root.join("firmware/zd1211")).unwrap(),
            Path::new(&format!("{web}/lib/firmware/zd1211"))
        );
    }

    #[test]
    fn microcode_goes_first_in_the_initrd_uncompressed() {
        let (_dir, lab, packages) = setup();
        let microcode = lab
            .on_host(&packages["kmod"].path)
            .join("kernel/x86/microcode");
        fs::create_dir_all(&microcode).unwrap();
        fs::write(microcode.join("AuthenticAMD.bin"), "ucode").unwrap();
        fs::create_dir_all(lab.on_host(&packages["web"].path).join("lib/firmware")).unwrap();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let mut model = with_hardware(model(80, false));
        model["base"]["kernel"]["microcode"] = json!({"package": "kmod", "image": "amd-ucode.img"});
        let outcome = realised(&lab, &mut db, &packages, &model);
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let inside = fs::read_link(root.join("initrd")).unwrap();
        let host = lab
            .work
            .join("store")
            .join(inside.strip_prefix("/wrath/store").unwrap());
        let initrd = fs::read(host).unwrap();
        let start = String::from_utf8_lossy(&initrd[..2048.min(initrd.len())]).into_owned();
        assert!(start.starts_with("070701"), "an uncompressed archive first");
        assert!(start.contains("kernel/x86/microcode/AuthenticAMD.bin"));
    }

    #[test]
    fn a_kernel_or_firmware_without_a_recipe_is_missing() {
        let (_dir, lab, packages) = setup();
        let mut model = with_hardware(model(80, false));
        model["base"]["kernel"]["package"] = json!("linux-latest");
        model["base"]["kernel"]["firmware"] = json!(["nofirmware"]);
        let lowered = lower(&lab, &model, &Kept::plain(&model), &packages).unwrap();
        assert!(
            lowered.skipped.contains(&"linux-latest".to_owned()),
            "{:?}",
            lowered.skipped
        );
        assert!(
            lowered.skipped.contains(&"nofirmware".to_owned()),
            "{:?}",
            lowered.skipped
        );
    }

    #[test]
    fn compiled_locales_are_where_locpath_points() {
        let (_dir, lab, packages) = setup();
        let locale = lab
            .on_host(&packages["web"].path)
            .join("lib/locale/en_US.UTF-8");
        fs::create_dir_all(&locale).unwrap();
        fs::write(locale.join("LC_CTYPE"), "").unwrap();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &model(80, false));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let link = root.join("lib/locale/en_US.UTF-8");
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
    }

    #[test]
    fn a_system_links_the_grapes_it_was_built_from() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &model(80, false));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        assert_eq!(
            fs::read_link(root.join("grapes")).unwrap(),
            PathBuf::from(packages["grapes"].path.inside())
        );
    }

    #[test]
    fn a_system_calls_itself_wrath() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &model(80, false));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let release = fs::read_to_string(root.join("etc/os-release")).unwrap();
        assert_eq!(release, "NAME=wrath\nID=wrath\nPRETTY_NAME=wrath\n");
    }

    #[test]
    fn a_system_whose_modules_name_it_keeps_their_name() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let mut model = model(80, false);
        let named = "NAME=\"Acme\"\nID=acme\nPRETTY_NAME=\"Acme OS\"\n";
        model["files"]["/etc/os-release"] = json!({"mode": 0o644, "text": named});
        let outcome = realised(&lab, &mut db, &packages, &model);
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        assert_eq!(
            fs::read_to_string(root.join("etc/os-release")).unwrap(),
            named
        );
        assert_eq!(crate::image::release_name(named), "Acme OS");
        assert_eq!(crate::image::release_name("NAME=Acme\n"), "Acme");
        assert_eq!(crate::image::release_name(""), "wrath");
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
        assert_eq!(read("etc/wrath/model.json"), kept.text);
        assert_eq!(read("etc/wrath/model-hash"), kept.hash);
        let built_from: BTreeMap<String, String> =
            serde_json::from_str(&read(PACKAGE_KEYS)).unwrap();
        assert_eq!(
            built_from.get("web"),
            Some(&packages["web"].unresolved),
            "the generation says which key each package it uses had"
        );
        assert!(built_from.contains_key("systemd"));
    }

    #[test]
    fn a_generation_boots_and_switches_through_wrath() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let outcome = realised(&lab, &mut db, &packages, &model(80, false));
        let root = lab.on_host(&StorePath::from_hash(&outcome.generation));
        let read = |path: &str| fs::read_to_string(root.join(path)).unwrap();
        assert!(read("init").contains("/bin/wrath\" activate --mode init \"$self\""));
        let switch = read("bin/switch-to-configuration");
        assert!(switch.contains("/bin/wrath\" activate --mode \"$1\" \"$self\""));
        let wrath = packages["wrath"].path.inside();
        assert_eq!(
            fs::read_link(root.join("bin/wrath")).unwrap(),
            Path::new(&format!("{wrath}/bin/wrath"))
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
        assert_eq!(params, "quiet loglevel=3\n");
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
        let found: Vec<String> = crate::fixup::files_under(&root)
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
    fn records_and_outputs_carry_no_provenance() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        realised(&lab, &mut db, &packages, &model(80, false));
        for query in ["SELECT record FROM actions", "SELECT output FROM outputs"] {
            let mut statement = db.0.prepare(query).unwrap();
            let rows: Vec<String> = statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert!(!rows.is_empty());
            for text in rows {
                assert!(!text.contains("files./etc/web.conf"), "{text}");
            }
        }
        for entry in fs::read_dir(&objects_of(&lab).0).unwrap() {
            let text = fs::read_to_string(entry.unwrap().path()).unwrap();
            assert!(!text.contains("files./etc/web.conf"), "{text}");
        }
    }

    #[test]
    fn an_action_whose_objects_were_lost_is_run_again() {
        let (_dir, lab, packages) = setup();
        let mut db = Db::open(&db_path(&lab)).unwrap();
        let first = realised(&lab, &mut db, &packages, &model(80, false));
        let objects = objects_of(&lab).0;
        let lost: Vec<PathBuf> = fs::read_dir(&objects)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        for object in &lost {
            fs::remove_file(object).unwrap();
        }
        let generation = lab.on_host(&StorePath::from_hash(&first.generation));
        fs::remove_dir_all(&generation).unwrap();

        let again = realised(&lab, &mut db, &packages, &model(80, false));

        assert_eq!(again.generation, first.generation);
        assert!(lost.iter().all(|object| object.is_file()));
        assert!(generation.join("etc/web.conf").is_file());
        assert!(fs::read_dir(&objects).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp")
        }));
    }

    #[test]
    fn an_action_keeps_its_key_when_it_comes_from_somewhere_else() {
        let (_dir, _lab, packages) = setup();
        let one = text("here", "etc/x", 0o644, "x".into(), &[], &packages).unwrap();
        let two = text("there", "etc/x", 0o644, "x".into(), &[], &packages).unwrap();
        assert_eq!(key_of(&one.resolved), key_of(&two.resolved));
    }
}
