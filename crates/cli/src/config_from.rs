//! Where the configuration a command acts on came from: a directory, or a URL fetched for this
//! command. A configuration from a URL is read as it is and its lock is never written. Each
//! generation keeps where its configuration came from, so `status` and `history` can say.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use serde_json::{Value, json};

/// Where a configuration came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigFrom {
    /// A directory on the machine the command ran on.
    Dir(PathBuf),
    /// A URL, with the `//dir` the configuration is in when it names one, and the commit
    /// fetched and its date when it names a repository.
    Url {
        url: String,
        rev: Option<String>,
        date: Option<String>,
    },
}

static FROM: OnceLock<ConfigFrom> = OnceLock::new();

/// Whether `given`, as `-C` takes it, is a URL rather than a directory.
pub fn is_url(given: &str) -> bool {
    ["github:", "https://", "http://", "path:"]
        .iter()
        .any(|scheme| given.starts_with(scheme))
}

/// `url` without its `//dir`, and the directory, when it names one.
fn split_dir(url: &str) -> (&str, Option<&str>) {
    let after_scheme = url.find("://").map_or(0, |at| at + 3);
    match url[after_scheme..].find("//") {
        Some(at) => {
            let at = after_scheme + at;
            let dir = url[at + 2..].trim_matches('/');
            (&url[..at], (!dir.is_empty()).then_some(dir))
        }
        None => (url, None),
    }
}

/// Fetches the configuration at `url`, its newest commit when it names a repository, and
/// returns the directory it's in. The command then acts on it, read-only.
pub fn fetch(url: &str) -> Result<PathBuf, String> {
    let (base, dir) = split_dir(url);
    // A directory is read where it is, and hashing it, which a source needs, could take long.
    let (rev, date, tree) = match base.strip_prefix("path:") {
        Some(dir) => (None, None, PathBuf::from(dir)),
        None => {
            let (source, tree) = crate::trees::follow(base)?;
            (source.rev, source.date, tree)
        }
    };
    let config = match dir {
        Some(dir)
            if Path::new(dir)
                .components()
                .all(|c| matches!(c, Component::Normal(_))) =>
        {
            tree.join(dir)
        }
        Some(dir) => return Err(format!("`{url}`: `{dir}` isn't a directory in it")),
        None => tree,
    };
    if !config.is_dir() {
        return Err(format!(
            "`{url}` has no directory `{}`",
            dir.unwrap_or_default()
        ));
    }
    set(ConfigFrom::Url {
        url: url.to_owned(),
        rev,
        date,
    });
    Ok(config)
}

/// Says where the configuration came from, once, when it's known before the command runs.
pub fn set(from: ConfigFrom) {
    let _ = FROM.set(from);
}

/// Where the configuration came from, when a command said.
pub fn get() -> Option<&'static ConfigFrom> {
    FROM.get()
}

/// The URL the configuration was fetched from, when it was.
pub fn fetched_url() -> Option<&'static str> {
    match get()? {
        ConfigFrom::Url { url, .. } => Some(url),
        ConfigFrom::Dir(_) => None,
    }
}

/// The option a configuration says where machines running it upgrade from with.
const FOLLOWS: &str = "updates.from";

/// Where machines running `model` upgrade from, when it says.
pub fn follows(model: &wrath_model::Model) -> Option<&str> {
    match model.options.get(FOLLOWS)? {
        wrath_eval::Value::Str(url) => Some(url),
        _ => None,
    }
}

/// Where the running system upgrades from, when it says.
pub fn running_follows() -> Option<String> {
    let model = crate::read_model(&crate::switching::running_model()?).ok()?;
    follows(&model).map(str::to_owned)
}

/// The file with the token machines running `model` read what they upgrade from with: a path, or
/// a secret's, which the machine decrypts.
pub fn token_file(model: &wrath_model::Model) -> Option<PathBuf> {
    use wrath_eval::{RuntimePart, Value};
    match model.options.get("updates.token")? {
        Value::Str(path) => Some(PathBuf::from(path)),
        Value::Runtime(parts) => match parts.as_slice() {
            [RuntimePart::Secret(name)] => Some(Path::new(wrath_eval::SECRETS_DIR).join(name)),
            _ => None,
        },
        _ => None,
    }
}

/// The running system's [`token_file`], when its configuration gives one.
pub fn running_token_file() -> Option<PathBuf> {
    let model = crate::read_model(&crate::switching::running_model()?).ok()?;
    token_file(&model)
}

/// The token the running system's configuration gives for reading what it upgrades from, when
/// it gives one and this user can read it.
pub fn running_token() -> Option<String> {
    let token = std::fs::read_to_string(running_token_file()?).ok()?;
    Some(token.trim().to_owned()).filter(|token| !token.is_empty())
}

/// What to say when a machine following `url` is asked to change its configuration or lock
/// with `verb`.
pub fn changed_elsewhere(url: &str, verb: &str) -> String {
    format!(
        "this machine upgrades from {url}, so its configuration and lock change there: run \
         `wrath {verb}` in a checkout of it and push, or give a configuration with -C"
    )
}

/// What to say when a configuration fetched from `url` would need its lock written.
pub fn unpinned(url: &str) -> String {
    format!(
        "the lock at {url} doesn't pin everything its configuration uses, and a configuration \
         fetched from a URL is read as it is; run `wrath check` in a checkout of it and push the \
         lock"
    )
}

impl ConfigFrom {
    /// As a generation keeps it.
    pub fn to_json(&self) -> Value {
        match self {
            Self::Dir(dir) => json!({ "dir": dir }),
            Self::Url { url, rev, date } => json!({ "url": url, "rev": rev, "date": date }),
        }
    }

    /// Whether it's the URL `url`, at any commit.
    pub fn is(&self, url: &str) -> bool {
        matches!(self, Self::Url { url: from, .. } if from == url)
    }

    /// As a command line argument, in hex, so no shell on the way needs it quoted.
    pub fn to_arg(&self) -> String {
        self.to_json()
            .to_string()
            .bytes()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    pub fn from_arg(arg: &str) -> Option<Self> {
        let bytes = (0..arg.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(arg.get(at..at + 2)?, 16).ok())
            .collect::<Option<Vec<u8>>>()?;
        Self::from_json(&serde_json::from_slice(&bytes).ok()?)
    }

    pub fn from_json(value: &Value) -> Option<Self> {
        let text = |key: &str| value[key].as_str().map(str::to_owned);
        match (text("url"), text("dir")) {
            (Some(url), _) => Some(Self::Url {
                url,
                rev: text("rev"),
                date: text("date"),
            }),
            (None, Some(dir)) => Some(Self::Dir(dir.into())),
            (None, None) => None,
        }
    }
}

impl std::fmt::Display for ConfigFrom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dir(dir) => write!(f, "{}", dir.display()),
            Self::Url { url, rev, date } => {
                write!(f, "{url}")?;
                if let Some(rev) = rev {
                    write!(f, " at {}", &rev[..rev.len().min(12)])?;
                }
                if let Some(date) = date {
                    write!(f, " ({date})")?;
                }
                Ok(())
            }
        }
    }
}

/// Where the machine at `root` keeps where each generation's configuration came from, by number.
const RECORDS: &str = "wrath/system/from";

/// Keeps where generation `number`'s configuration came from, when that's known.
pub fn keep(root: &Path, number: u32) -> Result<(), String> {
    get().map_or(Ok(()), |from| write(root, number, from))
}

fn record(root: &Path, number: u32) -> PathBuf {
    root.join(RECORDS).join(number.to_string())
}

fn write(root: &Path, number: u32, from: &ConfigFrom) -> Result<(), String> {
    let dir = root.join(RECORDS);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = record(root, number);
    std::fs::write(&path, format!("{}\n", from.to_json()))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Where generation `number`'s configuration came from, when the machine at `root` kept it.
pub fn of(root: &Path, number: u32) -> Option<ConfigFrom> {
    let text = std::fs::read_to_string(record(root, number)).ok()?;
    ConfigFrom::from_json(&serde_json::from_str(&text).ok()?)
}

/// Forgets where generation `number`'s configuration came from, once it's removed.
pub fn forget(root: &Path, number: u32) {
    let _ = std::fs::remove_file(record(root, number));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_is_a_secret_the_machine_decrypts_or_a_path() {
        use wrath_eval::{RuntimePart, Value};
        let mut model = wrath_model::Model::default();
        assert_eq!(token_file(&model), None);
        let secret = Value::Runtime(vec![RuntimePart::Secret("github-token".into())]);
        model.options.insert("updates.token".into(), secret);
        assert_eq!(
            token_file(&model),
            Some(PathBuf::from("/run/wrath/secrets/github-token"))
        );
        let path = Value::Str("/root/token".into());
        model.options.insert("updates.token".into(), path);
        assert_eq!(token_file(&model), Some(PathBuf::from("/root/token")));
    }

    #[test]
    fn a_double_slash_names_the_directory_a_configuration_is_in() {
        assert_eq!(
            split_dir("github:johnae/machines//framework"),
            ("github:johnae/machines", Some("framework"))
        );
        assert_eq!(
            split_dir("github:johnae/machines/stable//hosts/framework/"),
            ("github:johnae/machines/stable", Some("hosts/framework"))
        );
        assert_eq!(
            split_dir("https://example.com/machines.tar.gz//framework"),
            ("https://example.com/machines.tar.gz", Some("framework"))
        );
        assert_eq!(
            split_dir("https://example.com/machines.tar.gz"),
            ("https://example.com/machines.tar.gz", None)
        );
        assert_eq!(
            split_dir("github:johnae/machines"),
            ("github:johnae/machines", None)
        );
    }

    #[test]
    fn urls_and_directories_are_told_apart() {
        assert!(is_url("github:johnae/machines//framework"));
        assert!(is_url("https://example.com/m.tar.gz"));
        assert!(is_url("path:/srv/machines//framework"));
        assert!(!is_url("machines/framework"));
        assert!(!is_url("/etc/wrath"));
    }

    #[test]
    fn where_a_generation_came_from_is_kept_and_read_back() {
        let from = ConfigFrom::Url {
            url: "github:johnae/machines//framework".into(),
            rev: Some("0123456789abcdef0123".into()),
            date: Some("2026-11-16".into()),
        };
        assert_eq!(ConfigFrom::from_json(&from.to_json()), Some(from.clone()));
        assert_eq!(ConfigFrom::from_arg(&from.to_arg()), Some(from.clone()));
        assert_eq!(ConfigFrom::from_arg("7b7"), None);
        assert_eq!(
            from.to_string(),
            "github:johnae/machines//framework at 0123456789ab (2026-11-16)"
        );
        assert!(from.is("github:johnae/machines//framework"));
        assert!(!from.is("github:johnae/machines//desktop"));
        let dir = ConfigFrom::Dir("/home/john/machines/framework".into());
        assert!(!dir.is("github:johnae/machines//framework"));
        assert_eq!(ConfigFrom::from_json(&dir.to_json()), Some(dir));
        assert_eq!(ConfigFrom::from_json(&json!({})), None);
    }

    #[test]
    fn a_removed_generation_forgets_where_it_came_from() {
        let root = tempfile::tempdir().unwrap();
        let from = ConfigFrom::Dir("/etc/wrath".into());
        write(root.path(), 3, &from).unwrap();
        assert_eq!(of(root.path(), 3), Some(from));
        assert_eq!(of(root.path(), 4), None);
        forget(root.path(), 3);
        assert_eq!(of(root.path(), 3), None);
    }
}
