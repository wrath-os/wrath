//! Reads a `go.sum` into what a recipe needs: each module's zip and `go.mod` from the module
//! proxy, laid out as a proxy of files the build points `GOPROXY` at. Go checks each against the
//! `go.sum` itself when it builds, so the project's own lock is what vouches for them.

const PROXY: &str = "https://proxy.golang.org";

/// A file of a module the build needs, from the proxy.
#[derive(Debug, PartialEq, Eq)]
pub struct ModuleFile {
    pub url: String,
    /// Where it goes in the build's file proxy, relative to the source.
    pub into: String,
}

/// A module path as the proxy spells it, with each capital letter as `!` and the lower case one,
/// so paths stay apart on file systems that ignore case.
pub fn escape(path: &str) -> String {
    let mut escaped = String::with_capacity(path.len());
    for c in path.chars() {
        if c.is_ascii_uppercase() {
            escaped.push('!');
            escaped.push(c.to_ascii_lowercase());
        } else {
            escaped.push(c);
        }
    }
    escaped
}

/// The files a `go.sum` names: a `go.mod` for every module it lists, and the zip of each module
/// whose code it vouches for, each once. Lines that aren't `module version hash` are left out.
pub fn module_files(go_sum: &str) -> Vec<ModuleFile> {
    let mut files = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for line in go_sum.lines() {
        let mut fields = line.split_whitespace();
        let (Some(module), Some(version), Some(hash)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if !version.starts_with('v') || !hash.starts_with("h1:") {
            continue;
        }
        let (version, kind) = match version.strip_suffix("/go.mod") {
            Some(version) => (version, "mod"),
            None => (version, "zip"),
        };
        let path = format!("{}/@v/{version}.{kind}", escape(module));
        if seen.insert(path.clone()) {
            files.push(ModuleFile {
                url: format!("{PROXY}/{path}"),
                into: format!("goproxy/{path}"),
            });
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capitals_in_a_module_path_are_spelled_out() {
        assert_eq!(
            escape("github.com/BurntSushi/toml"),
            "github.com/!burnt!sushi/toml"
        );
        assert_eq!(escape("filippo.io/age"), "filippo.io/age");
    }

    #[test]
    fn a_go_sum_names_each_modules_go_mod_and_the_zips_it_vouches_for() {
        let go_sum = "\
filippo.io/edwards25519 v1.1.0 h1:FNf4tywRC1HmFuKW5xopWpigGjJKiJSV0Cqo0cJWDaA=
filippo.io/edwards25519 v1.1.0/go.mod h1:BxyFTGdWcka3PhytdK4V28tE5sGfRvvvRV7EaN4VDT4=
golang.org/x/sys v0.1.0/go.mod h1:oPkhp1MJrh7nUepCBck5+mAzfO9JrbApNNgaTdGDITg=

not a line of go.sum
";
        let files = module_files(go_sum);
        let into: Vec<&str> = files.iter().map(|file| file.into.as_str()).collect();
        assert_eq!(
            into,
            [
                "goproxy/filippo.io/edwards25519/@v/v1.1.0.zip",
                "goproxy/filippo.io/edwards25519/@v/v1.1.0.mod",
                "goproxy/golang.org/x/sys/@v/v0.1.0.mod",
            ]
        );
        assert_eq!(
            files[0].url,
            "https://proxy.golang.org/filippo.io/edwards25519/@v/v1.1.0.zip"
        );
    }
}
