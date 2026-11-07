//! Sources: part of the configuration's frame, read while loading like `include`.

use super::{err, ok, why};
use crate::MemFs;

const SYSTEM: &str = "[system]\nhostname = \"box\"\n";

#[test]
fn sources_are_read_from_any_local_file() {
    let evaluation = ok(&[
        (
            "system.toml",
            "[system]\nhostname = \"box\"\n\n[sources.stable]\nurl = \"github:NixOS/nixpkgs/nixos-25.11\"\n",
        ),
        (
            "services/edge.toml",
            "[sources.edge]\nurl = \"github:NixOS/nixpkgs/nixos-unstable\"\n\n\
             [sources.stable]\nurl = \"github:NixOS/nixpkgs/nixos-25.11\"\n",
        ),
    ]);

    let sources: Vec<String> = evaluation
        .declared_sources()
        .iter()
        .map(|(name, source)| {
            format!(
                "{name} {} ({})",
                source.url,
                evaluation.sources().location(source.at)
            )
        })
        .collect();
    assert_eq!(
        sources,
        [
            "edge github:NixOS/nixpkgs/nixos-unstable (services/edge.toml:2:7)",
            "stable github:NixOS/nixpkgs/nixos-25.11 (system.toml:5:7)",
        ]
    );
}

#[test]
fn error_bad_sources() {
    insta::assert_snapshot!(err(&[
        (
            "system.toml",
            "[system]\nhostname = \"box\"\n\n[sources.stable]\nurl = \"github:a/b\"\n\n\
             [sources.\"a#b\"]\nurl = \"github:c/d\"\n\n[sources.bad]\nurl = 3\nref = \"main\"\n\n\
             [sources.none]\n",
        ),
        ("services/other.toml", "[sources.stable]\nurl = \"github:x/y\"\n"),
        ("services/flat.toml", "sources = \"github:a/b\"\n"),
    ]), @r#"
    error: source name `a#b` can't have `#` in it
     --> system.toml:7:1
      |
    7 | [sources."a#b"]
      | ^^^^^^^^^^^^^^^ packages name a source as `NAME#package`

    error: unexpected `ref` in a source
      --> system.toml:12:1
       |
    12 | ref = "main"
       | ^^^ only `url` belongs here

    error: source `none` has no `url`
      --> system.toml:14:1
       |
    14 | [sources.none]
       | ^^^^^^^^^^^^^^ expected `url = "..."`

    error: `sources` is a table of sources by name
     --> services/flat.toml:1:11
      |
    1 | sources = "github:a/b"
      |           ^^^^^^^^^^^^ expected `[sources.NAME]`
      = help: for example `[sources.stable]` with `url = "github:..."`

    error: source `stable` is defined twice, differently
     --> system.toml:5:7
      |
    5 | url = "github:a/b"
      |       ^^^^^^^^^^^^ follows github:a/b
     --> services/other.toml:2:7
      |
    2 | url = "github:x/y"
      |       ^^^^^^^^^^^^ follows github:x/y
    "#);
}

#[test]
fn sources_belong_to_the_frame_not_a_home() {
    insta::assert_snapshot!(err(&[
        ("system.toml", SYSTEM),
        ("users/john.toml", "[sources.stable]\nurl = \"github:a/b\"\n"),
    ]), @"
    error: unknown option `users.john.sources`
     --> users/john.toml:1:2
      |
    1 | [sources.stable]
      |  ^^^^^^^ no option with this name
    ");
}

#[test]
fn why_says_where_a_source_is_declared() {
    let evaluation = ok(&[(
        "system.toml",
        "[system]\nhostname = \"box\"\n\n[sources.stable]\nurl = \"github:a/b\"\n",
    )]);

    insta::assert_snapshot!(why(&evaluation, "sources"), @r#"
    sources.stable = "github:a/b"
      set at system.toml:5:7
    "#);
    insta::assert_snapshot!(why(&evaluation, "sources.stable"), @r#"
    sources.stable = "github:a/b"
      set at system.toml:5:7
    "#);
}

/// Evaluates `files` with the source `acme`, at `github:acme/templates`, holding `acme`.
fn with_acme(files: &[(&str, &str)], acme: &[(&str, &str)]) -> Result<crate::Evaluation, String> {
    let config =
        MemFs::new(files.iter().copied()).with_source("acme", MemFs::new(acme.iter().copied()));
    super::eval_fs(&config)
}

const ACME: &str = "\n[sources.acme]\nurl = \"github:acme/templates\"\n";

#[test]
fn files_are_included_from_a_source() {
    let system = format!(
        "include = [{{ source = \"acme\", path = \"base.toml\", as = \"defaults\" }}]\n\n\
         [system]\nhostname = \"box\"\n{ACME}"
    );
    let evaluation = with_acme(
        &[("system.toml", &system)],
        &[
            (
                "base.toml",
                "include = [\"zone.toml\"]\n\n[system]\nmotd = \"hi\"\n",
            ),
            ("zone.toml", "[system]\ntimezone = \"Europe/Stockholm\"\n"),
        ],
    )
    .unwrap_or_else(|e| panic!("{e}"));

    insta::assert_snapshot!(why(&evaluation, "system"), @r#"
    system.hostname = "box"
      set at system.toml:4:12
    system.motd = "hi"
      default at acme:base.toml:4:8
    system.timezone = "Europe/Stockholm"
      default at acme:zone.toml:2:12
      overrides "UTC", default
    "#);
}

#[test]
fn error_includes_from_sources() {
    let system = format!(
        "include = [\n  {{ source = \"acme\", path = \"bad.toml\" }},\n  \
         {{ source = \"acme\", path = \"missing.toml\" }},\n  \
         {{ source = \"acmee\", path = \"x.toml\" }},\n]\n\n[system]\nhostname = \"box\"\n{ACME}"
    );
    let errors = with_acme(
        &[("system.toml", &system)],
        &[(
            "bad.toml",
            "include = [{ source = \"other\", path = \"x.toml\" }]\n\n\
             [sources.other]\nurl = \"github:o/x\"\n",
        )],
    )
    .expect_err("evaluation succeeded");
    insta::assert_snapshot!(errors, @r#"
    error: a file from source `acme` can't declare sources
     --> acme:bad.toml:3:2
      |
    3 | [sources.other]
      |  ^^^^^^^ declared here
      = help: declare it in the configuration that includes this file

    error: a file from source `acme` can't include files from `other`
     --> acme:bad.toml:1:23
      |
    1 | include = [{ source = "other", path = "x.toml" }]
      |                       ^^^^^^^ included here
      = help: include it from the configuration, which says which sources it uses

    error: cannot read `acme:missing.toml`: entity not found
     --> system.toml:3:29
      |
    3 |   { source = "acme", path = "missing.toml" },
      |                             ^^^^^^^^^^^^^^ included here

    error: there's no source `acmee`
     --> system.toml:4:14
      |
    4 |   { source = "acmee", path = "x.toml" },
      |              ^^^^^^^ included from here
      = help: did you mean `acme`?
    "#);
}

#[test]
fn error_a_source_that_cant_be_fetched() {
    let system = format!(
        "include = [{{ source = \"acme\", path = \"base.toml\" }}]\n\n\
         [system]\nhostname = \"box\"\n{ACME}"
    );
    let errors = super::eval_fs(&MemFs::new([("system.toml", system.as_str())]))
        .expect_err("evaluation succeeded");
    insta::assert_snapshot!(errors, @r#"
    error: cannot fetch source `acme`: no source `acme` in memory
     --> system.toml:1:23
      |
    1 | include = [{ source = "acme", path = "base.toml" }]
      |                       ^^^^^^ included from here
    "#);
}
