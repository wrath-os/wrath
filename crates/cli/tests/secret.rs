//! `wrath secret`: editing and re-encrypting secrets with age. Needs `age` on PATH.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FIRST: &str = "age1752tz55mdpnqrasdfja6pzw76jhqf532f2dhr5jpkngrfhz5ec0sax8ykl";
const SECOND: &str = "age1r0rfv9ulw3tjfymc8r08wh22pcvdmnx4k292cnj84nf7lhn4zcsqgphcah";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/configs/secrets")
        .join(name)
        .canonicalize()
        .unwrap()
}

/// A configuration in a temporary directory that declares `api-token` and encrypts to
/// `recipients`, and an editor script that runs `edit` with the file as `$1`.
struct Setup {
    dir: tempfile::TempDir,
}

impl Setup {
    fn new(recipients: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let recipients: Vec<String> = recipients.iter().map(|r| format!("{r:?}")).collect();
        let system = format!(
            "[system]\nhostname = \"box\"\nsecret_recipients = [{}]\n\n[secrets.api-token]\n",
            recipients.join(", ")
        );
        fs::write(dir.path().join("system.toml"), system).unwrap();
        Self { dir }
    }

    fn config(&self) -> &Path {
        self.dir.path()
    }

    fn secret(&self) -> PathBuf {
        self.config().join("secrets/api-token.age")
    }

    /// Runs `wrath secret` with an editor that runs the shell code `edit`.
    fn run(&self, args: &[&str], edit: &str) -> Output {
        let editor = self.config().join("editor.sh");
        fs::write(&editor, format!("#!/bin/sh\nset -e\n{edit}\n")).unwrap();
        Command::new(env!("CARGO_BIN_EXE_wrath"))
            .arg("secret")
            .args(args)
            .arg("--config")
            .arg(self.config())
            .arg("--identity")
            .arg(fixture("test-identity.txt"))
            .env("EDITOR", format!("sh {}", editor.display()))
            .env_remove("VISUAL")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .output()
            .unwrap()
    }

    fn decrypt(&self, identity: &str) -> String {
        let output = Command::new("age")
            .args(["-d", "-i"])
            .arg(fixture(identity))
            .arg(self.secret())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
}

fn ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

/// What a failed command printed after the line naming its configuration.
fn err(output: &Output) -> String {
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr.clone()).unwrap();
    let (context, rest) = stderr.split_once('\n').unwrap();
    assert!(context.starts_with("config "), "{stderr}");
    rest.to_owned()
}

#[test]
fn editing_a_new_secret_encrypts_what_was_written_to_the_recipients() {
    let setup = Setup::new(&[FIRST, SECOND]);

    let out = ok(&setup.run(&["edit", "api-token"], r#"printf 'token-one\n' > "$1""#));

    assert_eq!(out, "encrypted api-token to 2 recipients\n");
    assert_eq!(setup.decrypt("test-identity.txt"), "token-one\n");
    assert_eq!(setup.decrypt("second-identity.txt"), "token-one\n");
}

#[test]
fn editing_an_existing_secret_starts_from_its_contents() {
    let setup = Setup::new(&[FIRST]);
    ok(&setup.run(&["edit", "api-token"], r#"printf 'one\n' > "$1""#));

    let out = ok(&setup.run(&["edit", "api-token"], r#"printf 'two\n' >> "$1""#));

    assert_eq!(out, "encrypted api-token to 1 recipient\n");
    assert_eq!(setup.decrypt("test-identity.txt"), "one\ntwo\n");
}

#[test]
fn an_edit_that_changes_nothing_leaves_the_file_alone() {
    let setup = Setup::new(&[FIRST]);
    ok(&setup.run(&["edit", "api-token"], r#"printf 'one\n' > "$1""#));
    let before = fs::read(setup.secret()).unwrap();

    let out = ok(&setup.run(&["edit", "api-token"], "true"));

    assert_eq!(out, "api-token is unchanged\n");
    assert_eq!(fs::read(setup.secret()).unwrap(), before);
}

#[test]
fn rekeying_encrypts_every_secret_to_the_current_recipients() {
    let first = Setup::new(&[FIRST]);
    ok(&first.run(&["edit", "api-token"], r#"printf 'one\n' > "$1""#));
    let both = Setup::new(&[FIRST, SECOND]);
    fs::create_dir_all(both.secret().parent().unwrap()).unwrap();
    fs::copy(first.secret(), both.secret()).unwrap();

    let out = ok(&both.run(&["rekey"], "false"));

    assert_eq!(out, "encrypted api-token to 2 recipients\n");
    assert_eq!(both.decrypt("second-identity.txt"), "one\n");
}

#[test]
fn a_secret_must_be_declared_and_have_recipients() {
    let setup = Setup::new(&[FIRST]);
    insta::assert_snapshot!(err(&setup.run(&["edit", "api-tokn"], "false")), @"
    error: there is no secret `api-tokn`; the secrets are api-token
    help: declare it under `[secrets.api-tokn]` first
    ");

    let none = Setup::new(&[]);
    insta::assert_snapshot!(err(&none.run(&["edit", "api-token"], "false")), @"
    error: there is no one to encrypt to
    help: set `system.secret_recipients` to the machine's public host key and yours
    ");
}

#[test]
fn a_failed_edit_keeps_the_secret() {
    let setup = Setup::new(&[FIRST]);
    ok(&setup.run(&["edit", "api-token"], r#"printf 'one\n' > "$1""#));

    let out = setup.run(&["edit", "api-token"], r#"printf 'two\n' > "$1"; exit 1"#);

    insta::assert_snapshot!(err(&out), @"error: the editor failed (exit status: 1); api-token is unchanged");
    assert_eq!(setup.decrypt("test-identity.txt"), "one\n");
}
