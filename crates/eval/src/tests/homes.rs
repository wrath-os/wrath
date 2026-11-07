//! Homes: a user's own configuration, `home.toml`, rooted at their entry in `users`.

use crate::{
    ConfigKind, Input, ListMerge, MemFs, OptionSpec, Schema, Sources, Type, Value, evaluate,
};

use super::why;

/// What modules for homes declare: the user's packages, and their account's shell.
fn schema() -> Schema {
    let mut schema = Schema::default();
    let user = OptionSpec::new(Type::record([
        (
            "packages",
            OptionSpec::new(Type::list(Type::Str, ListMerge::Set)).default(Value::List(vec![])),
        ),
        ("shell", OptionSpec::new(Type::Str).optional().account()),
    ]));
    schema
        .declare("users", OptionSpec::new(Type::map(user)))
        .unwrap();
    schema
}

fn eval_home(files: &[(&str, &str)]) -> Result<crate::Evaluation, String> {
    evaluate(Input {
        sources: Sources::default(),
        config: &MemFs::new(files.iter().copied()),
        kind: ConfigKind::Home,
        user: Some("john"),
        role: None,
        schema: schema(),
        modules: &[],
    })
    .map_err(|e| e.to_string())
}

#[test]
fn a_home_configures_its_users_entry() {
    let evaluation = eval_home(&[
        (
            "home.toml",
            "include = [\"tools.toml\"]\npackages = [\"jq\"]\n",
        ),
        ("tools.toml", "packages = [\"ripgrep\"]\n"),
        // Nothing else in the directory is part of it.
        ("system.toml", "not = \"read\"\n"),
        ("users/jane.toml", "packages = [\"nope\"]\n"),
    ])
    .unwrap_or_else(|e| panic!("{e}"));

    insta::assert_snapshot!(why(&evaluation, "users"), @r#"
    users.john.packages = ["jq", "ripgrep"]
      set at home.toml:2:12: ["jq"]
      set at tools.toml:1:12: ["ripgrep"]
      overrides [], default
    "#);
}

#[test]
fn error_a_home_cant_set_the_account() {
    insta::assert_snapshot!(eval_home(&[("home.toml", "shell = \"fish\"\n")]).unwrap_err(), @r#"
    error: `users.john.shell` belongs to the account
     --> home.toml:1:1
      |
    1 | shell = "fish"
      | ^^^^^ set here
      = the machine's administrator sets it in the system configuration
    "#);
}

#[test]
fn error_a_home_without_home_toml() {
    insta::assert_snapshot!(eval_home(&[]).unwrap_err(), @"
    error: `home.toml` does not exist
     = help: a user's own configuration starts from `home.toml`
    ");
}
