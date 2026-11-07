//! Set and ordered lists: merging across files, anchors and removal.

use super::{eval, eval_with};
use crate::{OptionPath, explain};

const SYSTEM: (&str, &str) = ("system.toml", "[system]\nhostname = \"box\"\n");

const BASE_PAM: (&str, &str) = (
    "services/pam.toml",
    r#"
[[pam.login.auth]]
name = "env"
control = "required"
module = "pam_env.so"

[[pam.login.auth]]
name = "unix"
control = "sufficient"
module = "pam_unix.so"
after = "env"

[[pam.login.auth]]
name = "deny"
control = "required"
module = "pam_deny.so"
after = "unix"
"#,
);

fn why(files: &[(&str, &str)], option: &str) -> String {
    let evaluation = eval(files).unwrap_or_else(|e| panic!("evaluation failed:\n{e}"));
    explain(&evaluation, Some(&OptionPath::parse(option).unwrap())).unwrap()
}

fn err(files: &[(&str, &str)]) -> String {
    eval_with(files, &[]).expect_err("evaluation succeeded")
}

#[test]
fn ordered_items_from_several_files_follow_their_anchors() {
    insta::assert_snapshot!(why(&[
        SYSTEM,
        BASE_PAM,
        (
            "services/fprint.toml",
            "[[pam.login.auth]]\nname = \"fprint\"\ncontrol = \"sufficient\"\nmodule = \"pam_fprintd.so\"\nbefore = \"unix\"\nafter = \"env\"\n",
        ),
    ], "pam.login.auth"), @r#"
    pam.login.auth = [{ control = "required", module = "pam_env.so", name = "env" }, { control = "sufficient", module = "pam_fprintd.so", name = "fprint" }, { control = "sufficient", module = "pam_unix.so", name = "unix" }, { control = "required", module = "pam_deny.so", name = "deny" }]
      set at services/fprint.toml:1:1: [{ after = "env", before = "unix", control = "sufficient", module = "pam_fprintd.so", name = "fprint" }]
      set at services/pam.toml:2:1: [{ control = "required", module = "pam_env.so", name = "env" }, { after = "env", control = "sufficient", module = "pam_unix.so", name = "unix" }, { after = "unix", control = "required", module = "pam_deny.so", name = "deny" }]
      overrides [], default
    "#);
}

#[test]
fn removing_an_ordered_item_keeps_its_neighbours_in_order() {
    insta::assert_snapshot!(why(&[
        SYSTEM,
        BASE_PAM,
        ("services/no-unix.toml", "[pam.login]\nauth = { remove = [\"unix\"] }\n"),
    ], "pam.login.auth"), @r#"
    pam.login.auth = [{ control = "required", module = "pam_env.so", name = "env" }, { control = "required", module = "pam_deny.so", name = "deny" }]
      set at services/pam.toml:2:1
      overrides [], default
      removes ["unix"], set at services/no-unix.toml:2:8
    "#);
}

#[test]
fn removing_from_a_set_applies_whatever_level_added_the_item() {
    insta::assert_snapshot!(why(&[
        SYSTEM,
        ("users/john.toml", "packages = [\"helix\", \"nano\"]\n"),
        ("services/trim.toml", "[users.john]\npackages = { remove = [\"nano\"] }\n"),
    ], "users.john.packages"), @r#"
    users.john.packages = ["helix"]
      set at users/john.toml:1:12
      overrides [], default
      removes ["nano"], set at services/trim.toml:2:12
    "#);
}

#[test]
fn error_removing_something_that_is_not_there() {
    insta::assert_snapshot!(err(&[
        SYSTEM,
        ("users/john.toml", "packages = [\"helix\", \"nano\"]\n"),
        ("services/trim.toml", "[users.john]\npackages = { remove = [\"nanoo\"] }\n"),
    ]), @r#"
    error: cannot remove "nanoo" from `users.john.packages`, it is not there
     --> services/trim.toml:2:12
      |
    2 | packages = { remove = ["nanoo"] }
      |            ^^^^^^^^^^^^^^^^^^^^^^ removed here
      = help: did you mean `"nano"`?
    "#);
}

#[test]
fn error_removing_from_options_that_are_not_sets_or_ordered_lists() {
    insta::assert_snapshot!(err(&[
        ("system.toml", "[system]\nhostname = { remove = [\"box\"] }\nkernel_params = { remove = [\"quiet\"] }\n"),
    ]), @r#"
    error: only set and ordered lists support `add` and `remove`
     --> system.toml:2:12
      |
    2 | hostname = { remove = ["box"] }
      |            ^^^^^^^^^^^^^^^^^^^^ this option is a string

    error: only set and ordered lists support `add` and `remove`
     --> system.toml:3:17
      |
    3 | kernel_params = { remove = ["quiet"] }
      |                 ^^^^^^^^^^^^^^^^^^^^^^ this option is a list of items that are each a string
    "#);
}

#[test]
fn error_anchor_to_an_unknown_item() {
    insta::assert_snapshot!(err(&[
        SYSTEM,
        (
            "services/pam.toml",
            "[[pam.login.auth]]\nname = \"unix\"\ncontrol = \"sufficient\"\nmodule = \"pam_unix.so\"\nafter = \"evn\"\n",
        ),
    ]), @r#"
    error: `unix` is placed relative to `evn`, which is not in `pam.login.auth`
     --> services/pam.toml:1:1
      |
    1 | [[pam.login.auth]]
      | ^^^^^^^^^^^^^^^^^^ placed here
    "#);
}

#[test]
fn error_contradicting_anchors() {
    insta::assert_snapshot!(err(&[
        SYSTEM,
        BASE_PAM,
        (
            "services/loop.toml",
            "[[pam.login.auth]]\nname = \"early\"\ncontrol = \"optional\"\nmodule = \"pam_echo.so\"\nbefore = \"env\"\nafter = \"deny\"\n",
        ),
    ]), @r#"
    error: the `before` and `after` anchors in `pam.login.auth` contradict each other
     --> services/pam.toml:2:1
      |
    2 | [[pam.login.auth]]
      | ^^^^^^^^^^^^^^^^^^ `deny` placed here
     --> services/loop.toml:1:1
      |
    1 | [[pam.login.auth]]
      | ^^^^^^^^^^^^^^^^^^ `early` placed here
     --> services/pam.toml:2:1
      |
    2 | [[pam.login.auth]]
      | ^^^^^^^^^^^^^^^^^^ `env` placed here
     --> services/pam.toml:2:1
      |
    2 | [[pam.login.auth]]
      | ^^^^^^^^^^^^^^^^^^ `unix` placed here
    "#);
}

#[test]
fn error_two_different_items_with_one_name() {
    insta::assert_snapshot!(err(&[
        SYSTEM,
        BASE_PAM,
        (
            "services/other.toml",
            "[[pam.login.auth]]\nname = \"env\"\ncontrol = \"optional\"\nmodule = \"pam_env.so\"\n",
        ),
    ]), @r#"
    error: two different items named `env` in `pam.login.auth`
     --> services/other.toml:1:1
      |
    1 | [[pam.login.auth]]
      | ^^^^^^^^^^^^^^^^^^ one here
     --> services/pam.toml:2:1
      |
    2 | [[pam.login.auth]]
      | ^^^^^^^^^^^^^^^^^^ another here
    "#);
}

#[test]
fn error_ordered_item_without_a_name_or_with_unknown_fields() {
    insta::assert_snapshot!(err(&[
        SYSTEM,
        (
            "services/pam.toml",
            "[[pam.login.auth]]\ncontrol = \"required\"\nmodule = \"pam_env.so\"\n\n[[pam.other.auth]]\nname = \"x\"\ncontrol = \"required\"\nmodule = \"pam_x.so\"\nflags = 1\n",
        ),
    ]), @r#"
    error: invalid value for `pam.login.auth`
     --> services/pam.toml:1:1
      |
    1 | [[pam.login.auth]]
      | ^^^^^^^^^^^^^^^^^^ expected a table with a `name` string, found { control = "required", module = "pam_env.so" }

    error: invalid value for `pam.other.auth`
     --> services/pam.toml:5:1
      |
    5 | [[pam.other.auth]]
      | ^^^^^^^^^^^^^^^^^^ unknown field `flags`
    "#);
}

#[test]
fn a_file_adds_to_a_list_and_removes_from_it_at_once() {
    insta::assert_snapshot!(why(&[
        SYSTEM,
        (
            "users/john.toml",
            "include = [\"common.toml\"]\npackages = { add = [\"htop\"], remove = [\"git\"] }\n",
        ),
        ("common.toml", "packages = [\"git\", \"helix\"]\n"),
    ], "users.john.packages"), @r#"
    users.john.packages = ["helix", "htop"]
      set at users/john.toml:2:20: ["htop"]
      set at common.toml:1:12: ["git", "helix"]
      overrides [], default
      removes ["git"], set at users/john.toml:2:12
    "#);
}

#[test]
fn error_unknown_key_in_a_list_change() {
    insta::assert_snapshot!(err(&[
        ("users/john.toml", "packages = { add = [\"htop\"], replace = [\"git\"] }\n"),
        SYSTEM,
    ]), @r#"
    error: unexpected `replace` in a list change
     --> users/john.toml:1:30
      |
    1 | packages = { add = ["htop"], replace = ["git"] }
      |                              ^^^^^^^ only `add` and `remove` belong here
    "#);
}
