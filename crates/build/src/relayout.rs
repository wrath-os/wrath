//! Moving recipes from files of a theme, pinned in one `sources.lock`, to a file per package or
//! family with its lock beside it (decision 0037). A one-off: it goes once grapes has moved.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::chain::{self, Origin, Sources};
use crate::store::{Lab, with_path};

/// A package's text in a file of a theme: the comment right above its `[[package]]`, and
/// everything after it up to the next package's.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Piece {
    id: String,
    text: String,
}

/// `text`, a file of a theme, as what comes before its first package and each package's piece.
fn pieces(text: &str) -> Result<(String, Vec<Piece>), String> {
    let lines: Vec<&str> = text.lines().collect();
    let starts: Vec<usize> = (0..lines.len())
        .filter(|&n| lines[n].trim_end() == "[[package]]")
        .map(|header| {
            let above = lines[..header]
                .iter()
                .rev()
                .take_while(|line| line.trim_start().starts_with('#'))
                .count();
            header - above
        })
        .collect();
    let joined = |from: usize, to: usize| lines[from..to].join("\n").trim_end().to_owned();
    let head = joined(0, starts.first().copied().unwrap_or(lines.len()));
    let mut found = Vec::with_capacity(starts.len());
    for (n, &start) in starts.iter().enumerate() {
        let text = joined(start, starts.get(n + 1).copied().unwrap_or(lines.len()));
        #[derive(serde::Deserialize)]
        struct One {
            package: Vec<toml::Table>,
        }
        let parsed: One = toml::from_str(&text).map_err(|e| format!("{text}\n{e}"))?;
        let id = parsed.package[0]
            .get("id")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| format!("a package without an id:\n{text}"))?
            .to_owned();
        found.push(Piece { id, text });
    }
    Ok((head, found))
}

/// The family `id` belongs to among `ids`, the packages of one file: the family of the longest
/// id it starts with followed by `-`, such as `gtk4` for `gtk4-gir`, or its own.
fn family(id: &str, ids: &[String]) -> String {
    let mut front = id;
    while let Some((rest, _)) = front.rsplit_once('-') {
        if ids.iter().any(|other| other == rest) {
            return family(rest, ids);
        }
        front = rest;
    }
    id.to_owned()
}

/// The file a family's recipes go in, from `recipes/`: under the first two letters of its name.
fn file_of(family: &str) -> String {
    let shard = family
        .chars()
        .take(2)
        .collect::<String>()
        .to_ascii_lowercase();
    format!("{shard}/{family}.toml")
}

/// Moves every recipe in a file of a theme to the file of its package or family, its own files to
/// that file's directory and its pins to the lock beside it, then checks every package has the
/// key it had. Prints what no recipe has, such as the comment at the top of a file.
pub fn run(lab: &Lab, repository: &Path) -> Result<(), String> {
    let recipes = repository.join("recipes");
    let themes: Vec<String> = chain::recipe_files(repository)?
        .into_iter()
        .filter(|file| !file.contains('/'))
        .collect();
    if themes.is_empty() {
        return Err("every recipe is in a file of its own already".to_owned());
    }
    let mut packages = Vec::new();
    for file in &themes {
        packages.extend(chain::load_styled(lab, repository, file)?);
    }
    let mut before = chain::keys_of(lab, repository, packages.clone())?;
    // A package built from a tree of the repository, such as the set's own, changes with it.
    before.retain(|id, _| {
        let package = packages.iter().find(|package| package.id == *id);
        !package.is_some_and(|package| package.sources.iter().any(|s| s.url == "tree:."))
    });

    let mut texts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut file_of_id: BTreeMap<String, String> = BTreeMap::new();
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for theme in &themes {
        let path = recipes.join(theme);
        let text = fs::read_to_string(&path).map_err(with_path(&path))?;
        let (head, pieces) = pieces(&text)?;
        if !head.is_empty() {
            eprintln!("{theme} starts with what no recipe has:\n{head}\n");
        }
        let ids: Vec<String> = pieces.iter().map(|piece| piece.id.clone()).collect();
        for Piece { id, mut text } in pieces {
            let package = packages
                .iter()
                .find(|package| package.id == id)
                .ok_or_else(|| format!("`{id}` didn't load"))?;
            let file = file_of(&family(&id, &ids));
            let own = repository.join(Origin::of(&file, &id).dir);
            for name in &package.files {
                moved.push((recipes.join(name), own.join(name)));
                // A style reads a recipe's own file by its name among them.
                text = text.replace(&format!("\"recipes/{name}\""), &format!("\"{name}\""));
            }
            // The set's own package takes its files, and `sources.lock` is no longer one.
            text = text
                .replace("\"sources.lock\", ", "")
                .replace(" sources.lock ", " ");
            texts.entry(file.clone()).or_default().push(text);
            file_of_id.insert(id, file);
        }
    }
    for (file, texts) in &texts {
        let path = recipes.join(file);
        if path.exists() {
            return Err(format!("{} is there already", path.display()));
        }
        let dir = path.parent().expect("a recipe file is in a directory");
        fs::create_dir_all(dir).map_err(with_path(dir))?;
        fs::write(&path, texts.join("\n\n") + "\n").map_err(with_path(&path))?;
    }
    for (from, to) in &moved {
        let dir = to.parent().expect("a file is in a directory");
        fs::create_dir_all(dir).map_err(with_path(dir))?;
        fs::copy(from, to).map_err(with_path(from))?;
    }

    // Each package's sources, its style's included, pinned in its new lock the way fetching pins
    // them: by what `sources.lock` knows, or fetched when it doesn't.
    let mut sources = Sources::load(repository)?;
    for package in &packages {
        let lock = Origin::of(&file_of_id[&package.id], &package.id).lock;
        for source in &package.sources {
            sources.fetch_source(lab, source, &lock)?;
        }
    }

    let gone = themes.iter().map(|theme| recipes.join(theme));
    let gone = gone.chain(moved.iter().map(|(from, _)| from.clone()));
    for path in gone.chain([repository.join("sources.lock")]) {
        if path.exists() {
            fs::remove_file(&path).map_err(with_path(&path))?;
        }
    }

    let mut moved_packages = Vec::new();
    for file in chain::recipe_files(repository)? {
        moved_packages.extend(chain::load_styled(lab, repository, &file)?);
    }
    let after = chain::keys_of(lab, repository, moved_packages)?;
    let changed: Vec<&String> = before
        .iter()
        .filter(|(id, key)| after.get(*id) != Some(*key))
        .map(|(id, _)| id)
        .collect();
    if !changed.is_empty() {
        return Err(format!("these packages' keys changed: {changed:?}"));
    }
    println!("{} recipes in {} files", after.len(), texts.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEME: &str = "# Terminals.\n\n[[package]]\nid = \"ghostty\"\nscope = \"runtime\"\n\
                         script = \"true\"\n\n# Its manual.\n[[package]]\nid = \"ghostty-docs\"\n\
                         scope = \"runtime\"\nscript = \"true\"\n# Inside it.\n[package.x]\n";

    #[test]
    fn a_package_takes_the_comment_right_above_it() {
        let (head, found) = pieces(THEME).unwrap();
        assert_eq!(head, "# Terminals.");
        assert_eq!(found[0].id, "ghostty");
        assert!(found[0].text.starts_with("[[package]]\nid = \"ghostty\""));
        assert!(found[0].text.ends_with("script = \"true\""));
        assert_eq!(found[1].id, "ghostty-docs");
        assert!(found[1].text.starts_with("# Its manual.\n[[package]]"));
        assert!(found[1].text.ends_with("# Inside it.\n[package.x]"));
    }

    #[test]
    fn a_package_named_after_another_in_its_file_joins_its_family() {
        let ids: Vec<String> = ["gtk4", "gtk4-gir", "gtk4-gir-docs", "gtkmm", "m4-final"]
            .map(str::to_owned)
            .to_vec();
        assert_eq!(family("gtk4-gir", &ids), "gtk4");
        assert_eq!(family("gtk4-gir-docs", &ids), "gtk4");
        assert_eq!(family("gtkmm", &ids), "gtkmm");
        // `m4` is in another file.
        assert_eq!(family("m4-final", &ids), "m4-final");
    }

    #[test]
    fn a_family_goes_under_the_first_two_letters_of_its_name() {
        assert_eq!(file_of("libX11"), "li/libX11.toml");
        assert_eq!(file_of("jq"), "jq/jq.toml");
    }

    #[test]
    fn recipes_move_with_their_files_and_pins_and_keep_their_keys() {
        let dir = tempfile::tempdir().unwrap();
        let recipes = dir.path().join("recipes");
        fs::create_dir_all(recipes.join("locks")).unwrap();
        let tarball = dir.path().join("ghostty.tar.gz");
        fs::write(&tarball, "ghostty\n").unwrap();
        let url = format!("file://{}", tarball.display());
        let theme = format!(
            "[[package]]\nid = \"ghostty\"\nscope = \"runtime\"\nscript = \"true\"\n\
             files = [\"locks/g.lock\"]\nsources = [{{ url = \"{url}\", file = true, \
             into = \"g\" }}]\n\n[[package]]\nid = \"ghostty-docs\"\nscope = \"runtime\"\n\
             script = \"true\"\n"
        );
        fs::write(recipes.join("terminals.toml"), theme).unwrap();
        fs::write(recipes.join("locks/g.lock"), "own\n").unwrap();
        let hash = crate::seed::sha256_of(&tarball).unwrap();
        fs::write(dir.path().join("sources.lock"), format!("{hash}  {url}\n")).unwrap();
        let lab = Lab::new(dir.path().join("work"));
        // A seed taken as prepared, since keys only need its hash.
        let seed = "ab".repeat(32);
        fs::write(dir.path().join("seed.pin"), format!("{seed}\n")).unwrap();
        fs::create_dir_all(lab.work.join("seed")).unwrap();
        fs::write(lab.work.join(format!("seed/ready-{seed}")), "").unwrap();

        run(&lab, dir.path()).unwrap();

        assert_eq!(
            chain::recipe_files(dir.path()).unwrap(),
            ["gh/ghostty.toml"]
        );
        assert_eq!(
            fs::read_to_string(recipes.join("gh/ghostty/locks/g.lock")).unwrap(),
            "own\n"
        );
        let lock = fs::read_to_string(recipes.join("gh/ghostty.lock")).unwrap();
        assert!(lock.contains(&hash), "{lock}");
        for gone in [
            "recipes/terminals.toml",
            "recipes/locks/g.lock",
            "sources.lock",
        ] {
            assert!(!dir.path().join(gone).exists(), "{gone}");
        }
    }
}
