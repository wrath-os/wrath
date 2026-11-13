//! Projects: a configuration that starts from `wrath.toml`, or `wrath.local.toml` over it.

use crate::{ConfigKind, Input, MemFs, Sources, evaluate};

use super::{schema, why};

fn eval_project(files: &[(&str, &str)]) -> Result<crate::Evaluation, String> {
    evaluate(Input {
        sources: Sources::default(),
        config: &MemFs::new(files.iter().copied()),
        kind: ConfigKind::Project,
        user: None,
        role: None,
        schema: schema(),
        modules: &[],
    })
    .map_err(|e| e.to_string())
}

fn starting(evaluation: &crate::Evaluation) -> Vec<&str> {
    evaluation
        .sources()
        .files()
        .filter(|&file| evaluation.is_starting_file(file))
        .map(|file| evaluation.sources().name(file))
        .collect()
}

#[test]
fn a_project_is_its_wrath_toml_and_what_that_includes() {
    let evaluation = eval_project(&[
        ("wrath.toml", "include = [\"tools.toml\"]\n"),
        (
            "tools.toml",
            "[system]\ntimezone = \"Europe/Stockholm\"\nhostname = \"dev\"\n",
        ),
        // A project's own directories aren't part of its configuration.
        ("services/app.toml", "not = \"an option\"\n"),
        ("users/john.toml", "nor = \"this\"\n"),
    ])
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(starting(&evaluation), ["wrath.toml"]);
    insta::assert_snapshot!(why(&evaluation, "system.timezone"), @r#"
    system.timezone = "Europe/Stockholm"
      set at tools.toml:2:12
      overrides "UTC", default
    "#);
}

#[test]
fn a_local_file_starts_the_project_when_there_is_one() {
    let evaluation = eval_project(&[
        (
            "wrath.toml",
            "[system]\ntimezone = \"Europe/Stockholm\"\nhostname = \"dev\"\n",
        ),
        (
            "wrath.local.toml",
            "include = [{ path = \"wrath.toml\", as = \"defaults\" }]\n\n\
             [system]\ntimezone = \"Europe/Oslo\"\n",
        ),
    ])
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(starting(&evaluation), ["wrath.local.toml"]);
    insta::assert_snapshot!(why(&evaluation, "system"), @r#"
    system.hostname = "dev"
      default at wrath.toml:3:12
    system.timezone = "Europe/Oslo"
      set at wrath.local.toml:4:12
      overrides "Europe/Stockholm", default at wrath.toml:2:12
      overrides "UTC", default
    "#);
}

#[test]
fn error_a_project_without_wrath_toml() {
    insta::assert_snapshot!(eval_project(&[("system.toml", "")]).unwrap_err(), @"
    error: `wrath.toml` does not exist
     = help: a project starts from `wrath.toml`
    ");
}

#[test]
fn error_a_project_choosing_a_platform() {
    let errors = eval_project(&[("wrath.toml", "platform = \"wrath\"\n")]).unwrap_err();
    assert!(
        errors.contains("only a system chooses its platform"),
        "{errors}"
    );
    let config = MemFs::new([("wrath.toml", "platform = { source = \"acme\" }\n")]);
    assert_eq!(crate::platform(&config, None), Ok(crate::Platform::Wrath));
}
