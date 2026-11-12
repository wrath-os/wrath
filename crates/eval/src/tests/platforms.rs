//! Choosing the platform a system builds on.

use super::{SYSTEM, eval, ok};
use crate::{MemFs, Platform, platform};

const ACME: &str = "\n[sources.acme]\nurl = \"github:acme/platform\"\n";

fn chosen(files: &[(&str, &str)]) -> Result<Platform, String> {
    platform(&MemFs::new(files.iter().copied()), None)
}

#[test]
fn a_system_builds_on_distro_unless_its_starting_file_says_otherwise() {
    assert_eq!(chosen(&[("system.toml", SYSTEM)]), Ok(Platform::Distro));
    let distro = format!("platform = \"distro\"\n{SYSTEM}");
    assert_eq!(chosen(&[("system.toml", &distro)]), Ok(Platform::Distro));
    let acme = format!("platform = {{ source = \"acme\" }}\n{SYSTEM}{ACME}");
    let expected = Platform::Source {
        name: "acme".into(),
        url: "github:acme/platform".into(),
        path: "platform.toml".into(),
    };
    assert_eq!(chosen(&[("system.toml", &acme)]), Ok(expected.clone()));
    assert_eq!(ok(&[("system.toml", &acme)]).platform(), &expected);
    let elsewhere = format!(
        "platform = {{ source = \"acme\", path = \"kiosk/platform.toml\" }}\n{SYSTEM}{ACME}"
    );
    let Ok(Platform::Source { path, .. }) = chosen(&[("system.toml", &elsewhere)]) else {
        panic!("not a platform from a source");
    };
    assert_eq!(path, "kiosk/platform.toml");
}

#[test]
fn a_role_chooses_its_own_platform() {
    let kiosk = format!("platform = {{ source = \"acme\" }}\n{SYSTEM}{ACME}");
    let files = [
        ("roles/kiosk.toml", kiosk.as_str()),
        ("roles/desk.toml", SYSTEM),
    ];
    let config = MemFs::new(files.iter().copied());
    assert_eq!(platform(&config, Some("desk")), Ok(Platform::Distro));
    assert_eq!(platform(&config, Some("kiosk")).unwrap().name(), "acme");
}

#[test]
fn error_a_platform_that_isnt_built_in() {
    let system = format!("platform = \"acme\"\n{SYSTEM}");
    insta::assert_snapshot!(chosen(&[("system.toml", &system)]).unwrap_err(), @r#"
    error: there's no platform `acme`
     --> system.toml:1:12
      |
    1 | platform = "acme"
      |            ^^^^^^ only `distro` is built in
      = help: one from a source is `platform = { source = "acme" }`
    "#);
}

#[test]
fn error_a_platform_whose_source_isnt_declared() {
    let system = format!("platform = {{ source = \"acme\" }}\n{SYSTEM}");
    insta::assert_snapshot!(chosen(&[("system.toml", &system)]).unwrap_err(), @r#"
    error: the platform's source `acme` isn't declared
     --> system.toml:1:12
      |
    1 | platform = { source = "acme" }
      |            ^^^^^^^^^^^^^^^^^^^ used here
      = help: declare it in this file, as `[sources.acme]` with its `url`
    "#);
    let errors = eval(&[("system.toml", &system)]).unwrap_err();
    assert!(errors.contains("isn't declared"), "{errors}");
}

#[test]
fn error_a_platform_chosen_anywhere_but_the_starting_file() {
    let errors = eval(&[
        (
            "system.toml",
            &format!("include = [\"base.toml\"]\n{SYSTEM}"),
        ),
        ("base.toml", "platform = \"distro\"\n"),
    ])
    .unwrap_err();
    insta::assert_snapshot!(errors, @r#"
    error: `platform` is chosen in the file the configuration starts from
     --> base.toml:1:1
      |
    1 | platform = "distro"
      | ^^^^^^^^ chosen here
      = help: move it to `system.toml`, or to the role's file
    "#);
}
