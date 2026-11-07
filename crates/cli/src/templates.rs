//! Filling in files with secrets and machine values on the machine, as activation does: each
//! file's text around them comes from the system, the secrets from where they were decrypted, and
//! machine values from the machine itself.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::ExitCode;

/// Writes each file in `spec`, a JSON table of names to lists of parts, to `out`, readable by
/// root only; activation then gives it its owner and mode. A file that can't be filled in is
/// left out with an error, and the others are still written.
pub fn fill(
    spec: &Path,
    secrets: &Path,
    machine: &BTreeMap<String, String>,
    out: &Path,
) -> Result<ExitCode, String> {
    let text = std::fs::read_to_string(spec).map_err(|e| format!("{}: {e}", spec.display()))?;
    let templates: BTreeMap<String, Vec<BTreeMap<String, String>>> =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", spec.display()))?;
    let mut failed = false;
    for (name, parts) in &templates {
        let written = contents(parts, secrets, machine).and_then(|contents| {
            let path = out.join(name);
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .and_then(|mut file| file.write_all(contents.as_bytes()))
                .map_err(|e| format!("{}: {e}", path.display()))
        });
        if let Err(message) = written {
            eprintln!("cannot fill in {name}: {message}");
            failed = true;
        }
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// The text of a file from its parts, with each secret read from `secrets` without one trailing
/// newline, and each machine value from `machine`.
fn contents(
    parts: &[BTreeMap<String, String>],
    secrets: &Path,
    machine: &BTreeMap<String, String>,
) -> Result<String, String> {
    let mut out = String::new();
    for part in parts {
        let (kind, value) = part.iter().next().ok_or("a part is empty")?;
        match kind.as_str() {
            "text" => out.push_str(value),
            "secret" | "quoted_secret" => {
                let path = secrets.join(value);
                let secret = std::fs::read_to_string(&path)
                    .map_err(|e| format!("secret {value}: {}: {e}", path.display()))?;
                let secret = secret.strip_suffix('\n').unwrap_or(&secret);
                if kind == "quoted_secret" {
                    refuse_escapes(&format!("secret {value}"), secret)?;
                }
                out.push_str(secret);
            }
            "machine" | "quoted_machine" => {
                let filled = machine
                    .get(value)
                    .ok_or_else(|| format!("this machine has no value for `{value}`"))?;
                if kind == "quoted_machine" {
                    refuse_escapes(value, filled)?;
                }
                out.push_str(filled);
            }
            other => return Err(format!("a part can't be `{other}`")),
        }
    }
    Ok(out)
}

/// Refuses `value` when it would need escaping inside a quoted string of a file format. `what`
/// names it in the error.
fn refuse_escapes(what: &str, value: &str) -> Result<(), String> {
    match value
        .chars()
        .find(|c| matches!(c, '"' | '\'' | '\\') || c.is_control())
    {
        Some(c) => Err(format!(
            "{what} has {c:?} in it, which would need escaping where the file puts it"
        )),
        None => Ok(()),
    }
}

/// Where a machine keeps the role and values it was given at install.
const MACHINE_FILE: &str = "/var/lib/distro/machine.toml";

/// This machine's values, by dotted path as configurations refer to them: what it detects, and
/// its name, given at install or else its hostname.
pub fn machine_values() -> BTreeMap<String, String> {
    let cpus = std::thread::available_parallelism().map_or(1, usize::from);
    let mut values = BTreeMap::from([("facts.cpu.count".to_owned(), cpus.to_string())]);
    if let Some(name) = machine_name(
        Path::new(MACHINE_FILE),
        Path::new("/proc/sys/kernel/hostname"),
    ) {
        values.insert("machine.name".to_owned(), name);
    }
    values
}

/// The `name` in `machine_file`, or else the hostname read from `hostname`.
fn machine_name(machine_file: &Path, hostname: &Path) -> Option<String> {
    let given = std::fs::read_to_string(machine_file)
        .ok()
        .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
        .and_then(|doc| doc.get("name")?.as_str().map(str::to_owned));
    given.or_else(|| {
        let name = std::fs::read_to_string(hostname).ok()?;
        Some(name.trim().to_owned()).filter(|name| !name.is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_get_their_secrets_and_a_bad_one_doesnt_stop_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let (secrets, out) = (dir.path().join("secrets"), dir.path().join("out"));
        std::fs::create_dir_all(&secrets).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(secrets.join("token"), "s3cret\n").unwrap();
        std::fs::write(secrets.join("quoted"), "has \"quotes\"\n").unwrap();
        let spec = dir.path().join("spec.json");
        std::fs::write(
            &spec,
            r#"{
              "app.toml": [{"text": "token = \""}, {"quoted_secret": "token"}, {"text": "\"\n"}],
              "app.env": [{"text": "TOKEN="}, {"secret": "quoted"}, {"text": "\n"}],
              "bad.toml": [{"text": "x = \""}, {"quoted_secret": "quoted"}, {"text": "\"\n"}],
              "missing": [{"secret": "nope"}],
              "motd": [{"machine": "machine.name"}, {"text": "\n"}],
              "unknown": [{"quoted_machine": "machine.zone"}]
            }"#,
        )
        .unwrap();

        let machine = BTreeMap::from([("machine.name".to_owned(), "w-03".to_owned())]);
        let code = fill(&spec, &secrets, &machine, &out).unwrap();

        assert_eq!(code, ExitCode::FAILURE);
        let read = |name: &str| std::fs::read_to_string(out.join(name)).unwrap();
        assert_eq!(read("app.toml"), "token = \"s3cret\"\n");
        assert_eq!(read("app.env"), "TOKEN=has \"quotes\"\n");
        assert!(!out.join("bad.toml").exists());
        assert!(!out.join("missing").exists());
        assert_eq!(read("motd"), "w-03\n");
        assert!(!out.join("unknown").exists());
        let mode = std::fs::metadata(out.join("app.toml"))
            .unwrap()
            .permissions();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
            0o600
        );
    }
}
