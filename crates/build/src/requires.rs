//! What a dependency brings into a dependent's view: the packages its `.pc` files require
//! (decision 0019).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::chain::Built;
use crate::store::Lab;

/// Where packages keep their `.pc` files.
pub(crate) const PC_DIRS: [&str; 2] = ["lib/pkgconfig", "share/pkgconfig"];

/// What a built package says about what it brings along.
#[derive(Debug, Clone, Default)]
pub struct Provider {
    /// What it was built with, which its `.pc` files' modules are found among.
    pub deps: Vec<String>,
    /// What it brings that no `.pc` file names.
    pub propagates: Vec<String>,
    /// Whether its private requirements come along too.
    pub private: bool,
}

/// The packages one brings along, each with whether only privately.
pub type Requirements = BTreeMap<String, bool>;

/// The modules a `.pc` file's `Requires` and `Requires.private` name.
pub fn modules(text: &str) -> (Vec<String>, Vec<String>) {
    let mut variables = BTreeMap::new();
    let mut public = Vec::new();
    let mut private = Vec::new();
    // A line ending in a backslash goes on on the next, as pkg-config reads it.
    let joined = text.replace("\\\r\n", " ").replace("\\\n", " ");
    for line in joined.lines() {
        let line = line.trim();
        // A field is `Name: value`, a variable `name=value`, whichever separator comes first.
        let Some(at) = line.find([':', '=']) else {
            continue;
        };
        let (key, value) = (line[..at].trim(), expand(line[at + 1..].trim(), &variables));
        if line[at..].starts_with('=') {
            variables.insert(key.to_owned(), value);
            continue;
        }
        match key {
            "Requires" => public.extend(names(&value)),
            "Requires.private" => private.extend(names(&value)),
            _ => {}
        }
    }
    (public, private)
}

fn expand(value: &str, variables: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        let name = &rest[start + 2..start + end];
        out.push_str(variables.get(name).map_or("", String::as_str));
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

/// The module names in a requirement list such as `glib-2.0 >= 2.50, zlib`.
fn names(value: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut version_next = false;
    for token in value.split([',', ' ', '\t']).filter(|t| !t.is_empty()) {
        if version_next {
            version_next = false;
            continue;
        }
        let name = token.split(['<', '>', '=', '!']).next().unwrap_or_default();
        if name.is_empty() {
            // An operator on its own: the version follows.
            version_next = token.len() == operator_length(token);
            continue;
        }
        found.push(name.to_owned());
        // `name>=` with the version as the next token.
        version_next = name.len() < token.len()
            && name.len() + operator_length(&token[name.len()..]) == token.len();
    }
    found
}

fn operator_length(text: &str) -> usize {
    text.chars().take_while(|c| "<>=!".contains(*c)).count()
}

/// The `.pc` files of a package's outputs, each one's text by its module name.
fn pc_files(lab: &Lab, built: &Built, id: &str) -> Result<BTreeMap<String, String>, String> {
    let mut found = BTreeMap::new();
    for path in built.output_paths(id) {
        for (file, text) in crate::cache::pc_files(lab, &path)? {
            if let Some(module) = module_name(Path::new(&file)) {
                found.entry(module).or_insert(text);
            }
        }
    }
    Ok(found)
}

fn module_name(file: &Path) -> Option<String> {
    let name = file.file_name()?.to_str()?;
    name.strip_suffix(".pc").map(str::to_owned)
}

/// What package `id` brings into a dependent's view, followed transitively. A package that was
/// never built here, such as the seed, brings nothing.
pub fn requirements(lab: &Lab, built: &mut Built, id: &str) -> Result<Requirements, String> {
    if let Some(known) = built.requirements.get(id) {
        return Ok(known.clone());
    }
    let Some(provider) = built.providers.get(id).cloned() else {
        return Ok(Requirements::new());
    };
    let own = pc_files(lab, built, id)?;
    let mut providing = BTreeMap::new();
    for dep in &provider.deps {
        for module in pc_files(lab, built, dep)?.into_keys() {
            providing.entry(module).or_insert_with(|| dep.clone());
        }
    }
    let mut direct = Requirements::new();
    for (module, text) in &own {
        let (public, mut private) = modules(text);
        if !provider.private {
            private.clear();
        }
        let named = public
            .into_iter()
            .map(|name| (name, false))
            .chain(private.into_iter().map(|name| (name, true)));
        for (name, is_private) in named {
            if own.contains_key(&name) {
                continue;
            }
            let dep = providing.get(&name).ok_or_else(|| {
                format!(
                    "`{id}`'s {module}.pc requires `{name}`, which nothing `{id}` was built with \
                     provides"
                )
            })?;
            add(&mut direct, dep, is_private);
        }
    }
    for dep in &provider.propagates {
        if !provider.deps.contains(dep) {
            return Err(format!(
                "`{id}` propagates `{dep}`, which it isn't built with"
            ));
        }
        add(&mut direct, dep, false);
    }
    let mut all = direct.clone();
    for (dep, is_private) in direct {
        for (further, further_private) in requirements(lab, built, &dep)? {
            add(&mut all, &further, is_private || further_private);
        }
    }
    all.remove(id);
    built.requirements.insert(id.to_owned(), all.clone());
    Ok(all)
}

/// Adds `id`, which stays public if any way to it is.
fn add(requirements: &mut Requirements, id: &str, private: bool) {
    requirements
        .entry(id.to_owned())
        .and_modify(|known| *known &= private)
        .or_insert(private);
}

/// The headers `#include <...>` lines in `text` name.
fn included(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix('#')?.trim_start();
            let rest = rest
                .strip_prefix("include")?
                .trim_start()
                .strip_prefix('<')?;
            rest.split_once('>').map(|(name, _)| name)
        })
        .collect()
}

/// The directories under `include` a `.pc` file's `Cflags` add with `-I`, such as `glib-2.0`.
fn include_subdirs(text: &str) -> Vec<String> {
    let mut variables = BTreeMap::new();
    let mut found = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(at) = line.find([':', '=']) else {
            continue;
        };
        let (key, value) = (line[..at].trim(), expand(line[at + 1..].trim(), &variables));
        if line[at..].starts_with('=') {
            variables.insert(key.to_owned(), value);
        } else if key == "Cflags" {
            found.extend(value.split_whitespace().filter_map(|flag| {
                let dir = flag.strip_prefix("-I")?;
                let (_, sub) = dir.rsplit_once("/include/")?;
                let sub = sub.trim_end_matches('/');
                (!sub.is_empty()).then(|| sub.to_owned())
            }));
        }
    }
    found
}

/// Where a build finds package `id`'s headers: each output's `include`, and the directories in it
/// its `.pc` files add.
fn header_dirs(lab: &Lab, built: &Built, id: &str) -> Result<Vec<PathBuf>, String> {
    let mut subdirs = Vec::new();
    for text in pc_files(lab, built, id)?.values() {
        subdirs.extend(include_subdirs(text));
    }
    let mut dirs = Vec::new();
    for path in built.output_paths(id) {
        let include = lab.on_host(&path).join("include");
        if include.is_dir() {
            dirs.extend(subdirs.iter().map(|sub| include.join(sub)));
            dirs.push(include);
        }
    }
    Ok(dirs)
}

/// Lines for each header package `id` installs that includes one from a dependency with `.pc`
/// files that nothing `id` brings into a dependent's view provides: a dependent including that
/// header fails to compile, as noctalia did on libqalculate's `gmp.h`.
pub fn unbrought_headers(lab: &Lab, built: &mut Built, id: &str) -> Result<Vec<String>, String> {
    let Some(provider) = built.providers.get(id).cloned() else {
        return Ok(Vec::new());
    };
    let brought = requirements(lab, built, id)?;
    let mut candidates = Vec::new();
    for dep in &provider.deps {
        if !brought.contains_key(dep) && !pc_files(lab, built, dep)?.is_empty() {
            candidates.push((dep.clone(), header_dirs(lab, built, dep)?));
        }
    }
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let own = header_dirs(lab, built, id)?;
    let mut from: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut found = BTreeSet::new();
    for root in built.output_paths(id).iter().map(|path| lab.on_host(path)) {
        let include = root.join("include");
        if !include.is_dir() {
            continue;
        }
        for file in crate::fixup::files_under(&include)? {
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            let header = file
                .strip_prefix(&root)
                .unwrap_or(&file)
                .display()
                .to_string();
            for name in included(&text) {
                let dep = from.entry(name.to_owned()).or_insert_with(|| {
                    if own.iter().any(|dir| dir.join(name).is_file()) {
                        return None;
                    }
                    candidates
                        .iter()
                        .find(|(_, dirs)| dirs.iter().any(|dir| dir.join(name).is_file()))
                        .map(|(dep, _)| dep.clone())
                });
                if let Some(dep) = dep {
                    found.insert(format!("{header} includes <{name}> from `{dep}`"));
                }
            }
        }
    }
    Ok(found.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::StorePath;

    #[test]
    fn requires_names_modules_without_their_versions() {
        let (public, private) = modules(
            "prefix=/usr\nName: gtk\nRequires: gdk-3.0, atk >= 2.15.1 pango>=1.41 ,cairo\n\
             Requires.private: glib-2.0 >=2.57, zlib\nLibs: -lgtk-3\n",
        );
        assert_eq!(public, ["gdk-3.0", "atk", "pango", "cairo"]);
        assert_eq!(private, ["glib-2.0", "zlib"]);
    }

    #[test]
    fn requires_reads_variables_and_skips_what_isnt_a_requirement() {
        let (public, private) =
            modules("glib=glib-2.0\nDescription: a: b\nRequires: ${glib} = 2.80\nURL: x\n");
        assert_eq!(public, ["glib-2.0"]);
        assert!(private.is_empty());
        assert_eq!(modules("Name: none\n"), (Vec::new(), Vec::new()));
    }

    #[test]
    fn a_line_ending_in_a_backslash_goes_on_on_the_next() {
        let (public, private) = modules(
            "Requires: glib-2.0\nRequires.private: gio-2.0 >= 2.80 \\\n    freetype2 >= 2.13 \\\n\
             \x20   poppler = 26.11.0\n\nLibs: -lpoppler-glib\n",
        );
        assert_eq!(public, ["glib-2.0"]);
        assert_eq!(private, ["gio-2.0", "freetype2", "poppler"]);
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        lab: Lab,
        built: Built,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let lab = Lab::new(dir.path());
            Self {
                _dir: dir,
                lab,
                built: Built::default(),
            }
        }

        /// A built package with `.pc` files, each `(module, Requires, Requires.private)`.
        fn package(&mut self, id: &str, deps: &[&str], pcs: &[(&str, &str, &str)]) -> &mut Self {
            let path = StorePath::derive("test", &[id.as_bytes()]);
            let dir = self.lab.on_host(&path).join("lib/pkgconfig");
            fs::create_dir_all(&dir).unwrap();
            for (module, public, private) in pcs {
                let text = format!("Requires: {public}\nRequires.private: {private}\n");
                fs::write(dir.join(format!("{module}.pc")), text).unwrap();
            }
            self.built.paths.insert(id.to_owned(), path);
            self.built.providers.insert(
                id.to_owned(),
                Provider {
                    deps: deps.iter().map(|d| (*d).to_owned()).collect(),
                    propagates: Vec::new(),
                    private: true,
                },
            );
            self
        }

        fn requirements(&mut self, id: &str) -> Result<Vec<(String, bool)>, String> {
            Ok(requirements(&self.lab, &mut self.built, id)?
                .into_iter()
                .collect())
        }
    }

    fn found(pairs: &[(&str, bool)]) -> Vec<(String, bool)> {
        pairs.iter().map(|(id, p)| ((*id).to_owned(), *p)).collect()
    }

    /// gtk requires pango publicly and zlib privately; pango requires glib, which needs nothing.
    fn desktop() -> Fixture {
        let mut fixture = Fixture::new();
        fixture
            .package("zlib", &[], &[("zlib", "", "")])
            .package(
                "glib",
                &["zlib"],
                &[("glib-2.0", "", "zlib"), ("gio-2.0", "glib-2.0", "")],
            )
            .package("pango", &["glib"], &[("pango", "gio-2.0", "")])
            .package("gtk", &["pango", "zlib"], &[("gtk+-3.0", "pango", "zlib")]);
        fixture
    }

    #[test]
    fn a_package_brings_what_its_pc_files_require_followed_through() {
        let mut fixture = desktop();
        assert_eq!(
            fixture.requirements("gtk").unwrap(),
            found(&[("glib", false), ("pango", false), ("zlib", true)])
        );
        assert_eq!(
            fixture.requirements("glib").unwrap(),
            found(&[("zlib", true)])
        );
        assert!(fixture.requirements("zlib").unwrap().is_empty());
    }

    #[test]
    fn a_requirement_reached_publicly_any_way_is_public() {
        let mut fixture = desktop();
        fixture.package(
            "app-lib",
            &["glib", "zlib"],
            &[("app", "glib-2.0 zlib", "")],
        );
        assert_eq!(
            fixture.requirements("app-lib").unwrap(),
            found(&[("glib", false), ("zlib", false)])
        );
    }

    #[test]
    fn a_package_can_leave_its_private_requirements_out_and_add_what_no_pc_names() {
        let mut fixture = desktop();
        let gtk = fixture.built.providers.get_mut("gtk").unwrap();
        gtk.private = false;
        gtk.propagates = vec!["zlib".to_owned()];
        assert_eq!(
            fixture.requirements("gtk").unwrap(),
            found(&[("glib", false), ("pango", false), ("zlib", false)])
        );
    }

    #[test]
    fn a_requirement_nothing_provides_names_the_package_and_its_pc_file() {
        let mut fixture = Fixture::new();
        fixture.package("lonely", &[], &[("lonely", "missing >= 1", "")]);
        assert_eq!(
            fixture.requirements("lonely").unwrap_err(),
            "`lonely`'s lonely.pc requires `missing`, which nothing `lonely` was built with \
             provides"
        );
        fixture
            .built
            .providers
            .get_mut("lonely")
            .unwrap()
            .propagates = vec!["x".to_owned()];
        fixture.built.requirements.clear();
        fs::remove_file(
            fixture
                .lab
                .on_host(&fixture.built.paths["lonely"])
                .join("lib/pkgconfig/lonely.pc"),
        )
        .unwrap();
        assert_eq!(
            fixture.requirements("lonely").unwrap_err(),
            "`lonely` propagates `x`, which it isn't built with"
        );
    }

    #[test]
    fn a_package_built_elsewhere_brings_nothing() {
        let mut fixture = Fixture::new();
        assert!(fixture.requirements("seed").unwrap().is_empty());
    }

    impl Fixture {
        /// Writes `text` at `name` in package `id`.
        fn file(&mut self, id: &str, name: &str, text: &str) -> &mut Self {
            let path = self.lab.on_host(&self.built.paths[id]).join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
            self
        }
    }

    /// libqalculate's headers include gmp's, and its `.pc` file doesn't require gmp.
    fn qalculate() -> Fixture {
        let mut fixture = Fixture::new();
        fixture
            .package("gmp", &[], &[("gmp", "", "")])
            .file("gmp", "include/gmp.h", "")
            .package("glib", &[], &[("glib-2.0", "", "")])
            .file("glib", "include/glib-2.0/glib.h", "")
            .package(
                "qalc",
                &["gmp", "glib"],
                &[("libqalculate", "glib-2.0", "")],
            )
            .file(
                "qalc",
                "include/libqalculate/Number.h",
                "#include <gmp.h>\n#  include <glib.h>\n#include \"Calc.h\"\n\
                 #include <libqalculate/Calc.h>\n#include <stdio.h>\n",
            )
            .file("qalc", "include/libqalculate/Calc.h", "");
        fixture
    }

    #[test]
    fn a_header_including_what_the_package_doesnt_bring_is_named() {
        let mut fixture = qalculate();
        let lines = unbrought_headers(&fixture.lab, &mut fixture.built, "qalc").unwrap();
        assert_eq!(
            lines,
            ["include/libqalculate/Number.h includes <gmp.h> from `gmp`"]
        );
    }

    #[test]
    fn a_header_is_found_where_its_pc_file_points_and_nowhere_else() {
        let mut fixture = Fixture::new();
        fixture
            .package("glib", &[], &[("glib-2.0", "", "")])
            .file(
                "glib",
                "lib/pkgconfig/glib-2.0.pc",
                "prefix=/wrath/store/g\nincludedir=${prefix}/include\n\
                 Cflags: -I${includedir}/glib-2.0\n",
            )
            .file("glib", "include/glib-2.0/glib.h", "")
            .package("ffmpeg", &[], &[("libavutil", "", "")])
            .file("ffmpeg", "include/libavutil/time.h", "")
            .package("app", &["glib", "ffmpeg"], &[("app", "", "")])
            .file(
                "app",
                "include/app.h",
                "#include <glib.h>\n#include <time.h>\n",
            );
        let lines = unbrought_headers(&fixture.lab, &mut fixture.built, "app").unwrap();
        assert_eq!(lines, ["include/app.h includes <glib.h> from `glib`"]);
    }

    #[test]
    fn a_header_from_what_the_package_brings_along_is_fine() {
        let mut fixture = qalculate();
        fixture.built.providers.get_mut("qalc").unwrap().propagates = vec!["gmp".to_owned()];
        let lines = unbrought_headers(&fixture.lab, &mut fixture.built, "qalc").unwrap();
        assert!(lines.is_empty(), "{lines:?}");
    }
}
