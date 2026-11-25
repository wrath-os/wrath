//! Caches: part of the configuration's frame, read while loading like `[sources]`. Only a
//! system trusts keys.

use crate::{ConfigKind, Input, MemFs, Sources, evaluate};

use super::{err, ok, schema};

/// What the test schema needs of any configuration.
const HOST: &str = "[system]\nhostname = \"dev\"\n\n";

const KEY: &str = "home:a8bff5ac710872c25c1abc215d0f4b608ff18866c23b71066069e9f1d0cc7501";

fn caches(evaluation: &crate::Evaluation) -> Vec<String> {
    evaluation
        .declared_caches()
        .iter()
        .map(|(name, cache)| format!("{name} {} {:?}", cache.url, cache.keys))
        .collect()
}

#[test]
fn a_system_lists_caches_in_any_local_file_with_the_keys_it_trusts() {
    let evaluation = ok(&[
        (
            "system.toml",
            &format!(
                "[system]\nhostname = \"box\"\n\n[caches.home]\nurl = \"https://cache.lan\"\n\
                 keys = [\"{KEY}\"]\n"
            ),
        ),
        ("services/ci.toml", "[caches.ci]\nurl = \"/srv/cache\"\n"),
    ]);
    assert_eq!(
        caches(&evaluation),
        [
            "ci /srv/cache []".to_owned(),
            format!("home https://cache.lan [\"{KEY}\"]"),
        ]
    );
}

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

#[test]
fn a_project_lists_a_cache_by_its_url_and_trusts_no_keys() {
    let evaluation = eval_project(&[(
        "wrath.toml",
        &format!("{HOST}[caches.team]\nurl = \"https://c.example\"\n"),
    )])
    .unwrap();
    assert_eq!(caches(&evaluation), ["team https://c.example []"]);

    let error = eval_project(&[(
        "wrath.toml",
        &format!("{HOST}[caches.team]\nurl = \"https://c.example\"\nkeys = [\"{KEY}\"]\n"),
    )])
    .unwrap_err();
    insta::assert_snapshot!(error, @r#"
    error: only a system's configuration trusts a cache's keys
     --> wrath.toml:6:8
      |
    6 | keys = ["home:a8bff5ac710872c25c1abc215d0f4b608ff18866c23b71066069e9f1d0cc7501"]
      |        ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ trusted here
      = help: list the cache with its `url` alone; what it holds is fetched when the machine's own configuration trusts a key that signed it
    "#);
}

#[test]
fn error_bad_caches() {
    insta::assert_snapshot!(err(&[
        (
            "system.toml",
            "[system]\nhostname = \"box\"\n\n[caches.none]\n\n[caches.bad]\nurl = 3\n\n\
             [caches.typo]\nurl = \"/c\"\nkey = []\n\n[caches.short]\nurl = \"/c\"\n\
             keys = [\"home:abc\"]\n\n[caches.home]\nurl = \"/c\"\n",
        ),
        (
            "services/other.toml",
            "[caches.home]\nurl = \"/elsewhere\"\n"
        ),
        ("services/flat.toml", "caches = \"/c\"\n"),
        (
            "services/s3.toml",
            "[caches.bucket]\nurl = \"s3://bucket\"\n"
        ),
    ]));
}

#[test]
fn caches_are_read_before_anything_is_evaluated() {
    let fs = MemFs::new([
        (
            "system.toml",
            "[system]\nhostname = \"box\"\n\n[caches.home]\nurl = \"https://cache.lan\"\n",
        ),
        ("services/ci.toml", "[caches.ci]\nurl = \"/srv/cache\"\n"),
    ]);
    let declared = crate::declared_caches(&fs, None);
    let urls: Vec<(&str, &str)> = declared
        .iter()
        .map(|(name, cache)| (name.as_str(), cache.url.as_str()))
        .collect();
    assert_eq!(urls, [("ci", "/srv/cache"), ("home", "https://cache.lan")]);
}
