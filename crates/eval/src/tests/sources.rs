//! Sources: part of the configuration's frame, read while loading like `include`.

use super::{err, ok, why};

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
