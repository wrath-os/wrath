//! Filling in files with secrets on the machine, as activation does: each file's text around
//! its secrets comes from the system, and the secrets from where they were decrypted.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::ExitCode;

/// Writes each file in `spec`, a JSON table of names to lists of parts, to `out`, readable by
/// root only; activation then gives it its owner and mode. A file that can't be filled in is
/// left out with an error, and the others are still written.
pub fn fill(spec: &Path, secrets: &Path, out: &Path) -> Result<ExitCode, String> {
    let text = std::fs::read_to_string(spec).map_err(|e| format!("{}: {e}", spec.display()))?;
    let templates: BTreeMap<String, Vec<BTreeMap<String, String>>> =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", spec.display()))?;
    let mut failed = false;
    for (name, parts) in &templates {
        let written = contents(parts, secrets).and_then(|contents| {
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
/// newline.
fn contents(parts: &[BTreeMap<String, String>], secrets: &Path) -> Result<String, String> {
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
                if kind == "quoted_secret"
                    && let Some(c) = secret
                        .chars()
                        .find(|c| matches!(c, '"' | '\'' | '\\') || c.is_control())
                {
                    return Err(format!(
                        "secret {value} has {c:?} in it, which would need escaping where the \
                         file puts it"
                    ));
                }
                out.push_str(secret);
            }
            other => return Err(format!("a part can't be `{other}`")),
        }
    }
    Ok(out)
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
              "missing": [{"secret": "nope"}]
            }"#,
        )
        .unwrap();

        let code = fill(&spec, &secrets, &out).unwrap();

        assert_eq!(code, ExitCode::FAILURE);
        let read = |name: &str| std::fs::read_to_string(out.join(name)).unwrap();
        assert_eq!(read("app.toml"), "token = \"s3cret\"\n");
        assert_eq!(read("app.env"), "TOKEN=has \"quotes\"\n");
        assert!(!out.join("bad.toml").exists());
        assert!(!out.join("missing").exists());
        let mode = std::fs::metadata(out.join("app.toml"))
            .unwrap()
            .permissions();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
            0o600
        );
    }
}
