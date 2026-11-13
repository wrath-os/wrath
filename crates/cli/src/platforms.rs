//! The modules a configuration builds on: grapes', or those a platform from a source lists in
//! its `platform.toml`.

use std::path::{Component, Path, PathBuf};

use wrath_eval::{ConfigFs, Module, Platform, Sources};

use crate::system::{ModuleOrder, load_modules};

/// What a platform's `platform.toml` lists.
#[derive(Debug, PartialEq, Eq)]
struct Listed {
    /// Directories of the platform's own modules, relative to the file.
    modules: Vec<String>,
    /// Grapes' modules the platform keeps, by name.
    from_grapes: Vec<String>,
}

/// The modules for `config`, as the platform its starting file for `role` chooses, with grapes'
/// modules in `grapes`.
pub fn modules(
    config: &dyn ConfigFs,
    role: Option<&str>,
    grapes: &Path,
    sources: &mut Sources,
    order: ModuleOrder,
) -> Result<Vec<Box<dyn Module>>, String> {
    let Platform::Source { name, url, path } = wrath_eval::platform(config, role)? else {
        return load_modules(grapes, sources, order);
    };
    let tree = config
        .source(&name, &url)
        .map_err(|e| format!("cannot fetch the platform's source `{name}`: {e}"))?;
    let root = tree
        .dir()
        .ok_or_else(|| format!("the platform's source `{name}` isn't on disk"))?;
    let file = inside(root, &path)
        .ok_or_else(|| format!("the platform's `path` must stay inside `{name}`: {path}"))?;
    let shown = format!("{name}: {path}");
    let text = std::fs::read_to_string(&file)
        .map_err(|e| format!("{shown}: {e}; a platform lists its modules in it"))?;
    let listed = listed(&text).map_err(|e| format!("{shown}: {e}"))?;
    let base = Path::new(&path).parent().unwrap_or(Path::new(""));
    let mut modules = Vec::new();
    for dir in &listed.modules {
        let dir = inside(root, base.join(dir)).ok_or_else(|| {
            format!("{shown}: module directory `{dir}` must stay inside `{name}`")
        })?;
        modules.extend(load_modules(&dir, sources, order)?);
    }
    modules.extend(from_grapes(grapes, &listed.from_grapes, sources, order)?);
    Ok(modules)
}

/// Grapes' modules named in `names`.
fn from_grapes(
    grapes: &Path,
    names: &[String],
    sources: &mut Sources,
    order: ModuleOrder,
) -> Result<Vec<Box<dyn Module>>, String> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let all = load_modules(grapes, sources, order)?;
    let known: Vec<&str> = all.iter().map(|module| module.name()).collect();
    if let Some(unknown) = names.iter().find(|name| !known.contains(&name.as_str())) {
        let hint = wrath_eval::closest(unknown, known.iter().copied())
            .map(|close| format!("; did you mean `{close}`?"))
            .unwrap_or_default();
        return Err(format!("grapes has no module `{unknown}`{hint}"));
    }
    Ok(all
        .into_iter()
        .filter(|module| names.iter().any(|name| name == module.name()))
        .collect())
}

/// Reads a `platform.toml`.
fn listed(text: &str) -> Result<Listed, String> {
    let document: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
    if let Some((key, _)) = document
        .iter()
        .find(|(key, _)| !["modules", "from-grapes"].contains(key))
    {
        return Err(format!(
            "unknown key `{key}`; a platform has `modules` and `from-grapes`"
        ));
    }
    let strings = |key: &str| -> Result<Vec<String>, String> {
        let Some(item) = document.get(key) else {
            return Ok(Vec::new());
        };
        let list = item
            .as_array()
            .ok_or_else(|| format!("`{key}` is a list of strings"))?;
        list.iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("`{key}` is a list of strings"))
            })
            .collect()
    };
    Ok(Listed {
        modules: strings("modules")?,
        from_grapes: strings("from-grapes")?,
    })
}

/// `relative` under `root`, when it doesn't climb out of it.
fn inside(root: &Path, relative: impl AsRef<Path>) -> Option<PathBuf> {
    let path = relative.as_ref();
    let stays = path
        .components()
        .all(|part| matches!(part, Component::Normal(_) | Component::CurDir));
    stays.then(|| root.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_platform_lists_its_own_module_directories_and_wrath_modules_by_name() {
        let text = "modules = [\"modules\"]\nfrom-grapes = [\"dbus\", \"users\"]\n";
        assert_eq!(
            listed(text).unwrap(),
            Listed {
                modules: vec!["modules".into()],
                from_grapes: vec!["dbus".into(), "users".into()],
            }
        );
        assert_eq!(listed("").unwrap().modules, Vec::<String>::new());
    }

    #[test]
    fn error_a_platform_file_with_something_else_in_it() {
        assert!(
            listed("modules = \"modules\"\n")
                .unwrap_err()
                .contains("list of strings")
        );
        assert!(
            listed("module = []\n")
                .unwrap_err()
                .contains("unknown key `module`")
        );
    }

    /// A system on acme's platform: acme's own `motd` module and grapes' `users`, with the
    /// lock pinning acme. Gives the configuration's directory and acme's.
    fn on_acme(from_grapes: &str) -> (tempfile::TempDir, tempfile::TempDir) {
        let acme = tempfile::tempdir().unwrap();
        let platform = format!("modules = [\"modules\"]\nfrom-grapes = [{from_grapes}]\n");
        std::fs::write(acme.path().join("platform.toml"), platform).unwrap();
        std::fs::create_dir(acme.path().join("modules")).unwrap();
        let motd = "def render(ctx: Context) -> None:\n    \
                    ctx.file(\"/etc/motd\", ctx.read(\"motd\") + \"\\n\")\n\n\
                    module(\n    name = \"motd\",\n    \
                    options = {\"motd\": option(types.str, default = \"acme\")},\n    \
                    writes = [],\n    render = render,\n)\n";
        std::fs::write(acme.path().join("modules/motd.star"), motd).unwrap();

        let config = tempfile::tempdir().unwrap();
        let url = format!("path:{}", acme.path().display());
        let system = format!(
            "platform = {{ source = \"acme\" }}\nmotd = \"hello\"\n\n\
             [users.ann]\n\n[sources.acme]\nurl = \"{url}\"\n"
        );
        std::fs::write(config.path().join("system.toml"), system).unwrap();
        let (source, _) = crate::trees::follow(&url).unwrap();
        let mut lock = wrath_model::Lock::default();
        lock.sources.insert("acme".into(), source);
        std::fs::write(config.path().join("wrath.lock"), lock.to_toml()).unwrap();
        (config, acme)
    }

    fn grapes_modules() -> PathBuf {
        crate::places::checkout_for_tests().join("modules")
    }

    #[test]
    fn a_system_on_a_platform_has_its_modules_and_the_wrath_ones_it_keeps() {
        let (config, _acme) = on_acme("\"users\"");
        let evaluation = crate::system::evaluate_dir(
            config.path(),
            &grapes_modules(),
            ModuleOrder::Sorted,
            None,
        )
        .unwrap();
        let names: Vec<&str> = evaluation
            .modules()
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(names, ["motd", "users"]);
        let model = wrath_model::Model::from_evaluation(&evaluation);
        assert_eq!(model.files["/etc/motd"].text, "hello\n");
        assert_eq!(model.platform.as_deref(), Some("acme"));
    }

    #[test]
    fn error_a_platform_keeping_a_module_wrath_doesnt_have() {
        let (config, _acme) = on_acme("\"userz\"");
        let error = crate::system::evaluate_dir(
            config.path(),
            &grapes_modules(),
            ModuleOrder::Sorted,
            None,
        )
        .unwrap_err();
        assert_eq!(error, "grapes has no module `userz`; did you mean `users`?");
    }

    #[test]
    fn paths_in_a_platform_stay_inside_its_source() {
        let root = Path::new("/src");
        assert_eq!(inside(root, "a/b"), Some(PathBuf::from("/src/a/b")));
        assert_eq!(inside(root, "../x"), None);
        assert_eq!(inside(root, "/etc"), None);
    }
}
