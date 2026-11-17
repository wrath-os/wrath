//! Automatic upgrades, which `wrath-upgrade.timer` runs as `wrath apply --automatic`: the
//! newest commit of what the machine follows, switched to now or at the next boot as the
//! configuration's `updates.switch` says. Each run's outcome is kept for `status`, and runs
//! pause while the system came from somewhere else, such as a checkout applied by hand. A failure
//! or a pause is also shown as people log in, from a notice PAM reads.

use std::path::Path;
use std::process::ExitCode;

use serde_json::json;
use wrath_model::{Model, Tier};

use crate::config_from::ConfigFrom;

/// Where the machine at `root` keeps the outcome of the last automatic upgrade.
const OUTCOME: &str = "wrath/system/upgrade.json";

/// What logins show about automatic upgrades, there only while there's something to say. grapes'
/// `updates` module has PAM show it.
const NOTICE: &str = "wrath/system/notice";

/// What an automatic upgrade does once it has the new system, as `updates.switch` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Switch now when every change takes effect live or at the next login, else at boot.
    Auto,
    Now,
    Boot,
}

/// The mode `model` asks automatic upgrades for.
pub fn mode(model: &Model) -> Mode {
    match model.options.get("updates.switch") {
        Some(wrath_eval::Value::Str(mode)) if mode == "now" => Mode::Now,
        Some(wrath_eval::Value::Str(mode)) if mode == "boot" => Mode::Boot,
        _ => Mode::Auto,
    }
}

/// Whether automatic upgrades to `model` may build what no cache has, as `updates.build` says.
pub fn builds(model: &Model) -> bool {
    !matches!(
        model.options.get("updates.build"),
        Some(wrath_eval::Value::Bool(false))
    )
}

/// Why this automatic upgrade waited instead of building, which [`finish`] keeps.
static WAITED: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Leaves this automatic upgrade for a later run, as it would build `builds`, such as
/// `build 2 (jq; 1 built with it)`, and building is left to a cache.
pub fn wait(builds: &str) -> ExitCode {
    let why = format!("it would {builds}, and `updates.build` is off, so it waits for a cache");
    println!("not upgrading yet: {why}");
    let _ = WAITED.set(why);
    ExitCode::SUCCESS
}

/// Whether an automatic upgrade in `mode`, whose changes take effect at `tier`, switches now
/// rather than at the next boot.
pub fn switches_now(mode: Mode, tier: Option<Tier>) -> bool {
    match mode {
        Mode::Now => true,
        Mode::Boot => false,
        Mode::Auto => tier.is_none_or(|tier| tier <= Tier::NextLogin),
    }
}

/// How the last automatic upgrade went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// When, as `2026-11-16 03:12:09` in UTC.
    pub at: String,
    pub kind: Kind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Done,
    /// Not upgraded yet: it would build, which `updates.build = false` leaves to a cache.
    Waiting,
    Paused,
    Failed,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Waiting => "waiting",
            Self::Paused => "paused",
            Self::Failed => "failed",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        [Self::Done, Self::Waiting, Self::Paused, Self::Failed]
            .into_iter()
            .find(|kind| kind.name() == name)
    }
}

impl Outcome {
    pub fn to_json(&self) -> serde_json::Value {
        json!({ "at": self.at, "kind": self.kind.name(), "message": self.message })
    }
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            Kind::Done => write!(
                f,
                "the last automatic upgrade, {}: {}",
                self.at, self.message
            ),
            Kind::Waiting => write!(
                f,
                "the last automatic upgrade waited, {}: {}",
                self.at, self.message
            ),
            Kind::Paused => write!(
                f,
                "automatic upgrades are paused since {}: {}",
                self.at, self.message
            ),
            Kind::Failed => write!(
                f,
                "the last automatic upgrade failed, {}: {}",
                self.at, self.message
            ),
        }
    }
}

/// Keeps `kind` with `message` as the outcome of the automatic upgrade that ran now. A pause
/// keeps the time it started.
pub fn keep(root: &Path, kind: Kind, message: &str) -> Result<(), String> {
    let at = match last(root) {
        Some(last) if kind == Kind::Paused && last.kind == Kind::Paused => last.at,
        _ => crate::switching::now_utc(),
    };
    let path = root.join(OUTCOME);
    let outcome = Outcome {
        at,
        kind,
        message: message.to_owned(),
    };
    let text = outcome.to_json().to_string();
    std::fs::write(&path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
    write_notice(root, Some(&outcome))
}

/// Shows at login what's left to say once the machine at `root` switched by hand to a system
/// whose configuration came from `from`: nothing when that's `follows`, as the machine is up to
/// date with it whatever upgrading on its own did before, and otherwise what [`shown`] says.
pub fn applied_by_hand(
    root: &Path,
    follows: &str,
    from: Option<&ConfigFrom>,
) -> Result<(), String> {
    if from.is_some_and(|from| from.is(follows)) {
        return write_notice(root, None);
    }
    write_notice(root, shown(root, Some(follows), from).as_ref())
}

fn write_notice(root: &Path, outcome: Option<&Outcome>) -> Result<(), String> {
    let path = root.join(NOTICE);
    let failed = |e: std::io::Error| format!("{}: {e}", path.display());
    match outcome.filter(|outcome| matches!(outcome.kind, Kind::Failed | Kind::Paused)) {
        Some(outcome) => std::fs::write(&path, format!("wrath: {outcome}\n")).map_err(failed),
        None => match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(failed(e)),
            _ => Ok(()),
        },
    }
}

/// How the last automatic upgrade on the machine at `root` went, when one ran.
pub fn last(root: &Path) -> Option<Outcome> {
    let text = std::fs::read_to_string(root.join(OUTCOME)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(Outcome {
        at: value["at"].as_str()?.to_owned(),
        kind: Kind::from_name(value["kind"].as_str()?)?,
        message: value["message"].as_str()?.to_owned(),
    })
}

/// The last outcome, to show on a machine that upgrades from `follows`, whose current generation's
/// configuration came from `from`. A pause no longer holds once the machine is back on `follows`,
/// or follows nothing.
pub fn shown(root: &Path, follows: Option<&str>, from: Option<&ConfigFrom>) -> Option<Outcome> {
    last(root).filter(|outcome| {
        outcome.kind != Kind::Paused || follows.is_some_and(|f| paused(f, from).is_some())
    })
}

/// Why automatic upgrades from `follows` pause, when the running system's configuration, as
/// `from` says, came from somewhere else.
pub fn paused(follows: &str, from: Option<&ConfigFrom>) -> Option<String> {
    let from = from.filter(|from| !from.is(follows))?;
    Some(format!(
        "the system came from {from}; `wrath apply` goes back to {follows} and resumes them"
    ))
}

/// Says and keeps that automatic upgrades from `follows` are paused, when they are, before
/// anything is fetched. `from` is where the running system's configuration came from.
pub fn pause(root: &Path, follows: &str, from: Option<&ConfigFrom>) -> Result<bool, String> {
    let Some(why) = paused(follows, from) else {
        return Ok(false);
    };
    println!("not upgrading: {why}");
    keep(root, Kind::Paused, &why)?;
    Ok(true)
}

/// Keeps how an automatic upgrade from `follows` went, as `result` says, and passes it on.
pub fn finish(
    root: &Path,
    follows: &str,
    result: Result<ExitCode, String>,
) -> Result<ExitCode, String> {
    let (kind, message) = match &result {
        Ok(code) if *code != ExitCode::SUCCESS => (
            Kind::Failed,
            "it stopped with errors; `journalctl -u wrath-upgrade` has them".to_owned(),
        ),
        Ok(_) if let Some(why) = WAITED.get() => (Kind::Waiting, why.clone()),
        Ok(_) => (
            Kind::Done,
            match crate::config_from::get() {
                Some(from) => format!("up to date with {from}"),
                None => format!("up to date with {follows}"),
            },
        ),
        Err(message) => (Kind::Failed, message.clone()),
    };
    keep(root, kind, &message)?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_waits_for_a_boot_only_for_changes_that_need_one() {
        for (tier, now) in [
            (None, true),
            (Some(Tier::Live), true),
            (Some(Tier::NextLogin), true),
            (Some(Tier::SoftReboot), false),
            (Some(Tier::Reboot), false),
        ] {
            assert_eq!(switches_now(Mode::Auto, tier), now, "{tier:?}");
        }
        assert!(switches_now(Mode::Now, Some(Tier::Reboot)));
        assert!(!switches_now(Mode::Boot, Some(Tier::Live)));
    }

    #[test]
    fn upgrades_pause_while_the_system_came_from_elsewhere() {
        let follows = "github:johnae/machines//framework";
        let there = ConfigFrom::Url {
            url: follows.into(),
            rev: Some("abc".into()),
            date: None,
        };
        let checkout = ConfigFrom::Dir("/home/john/machines/framework".into());
        assert_eq!(paused(follows, Some(&there)), None);
        // A generation from before origins were kept doesn't stop upgrades.
        assert_eq!(paused(follows, None), None);
        let why = paused(follows, Some(&checkout)).unwrap();
        assert!(why.contains("/home/john/machines/framework"), "{why}");
    }

    #[test]
    fn a_pause_shows_only_while_the_system_still_came_from_elsewhere() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("wrath/system")).unwrap();
        let follows = "github:johnae/machines//framework";
        let there = ConfigFrom::Url {
            url: follows.into(),
            rev: None,
            date: None,
        };
        let checkout = ConfigFrom::Dir("/home/john/machines/framework".into());
        keep(root.path(), Kind::Paused, "a checkout").unwrap();
        assert!(shown(root.path(), Some(follows), Some(&checkout)).is_some());
        assert_eq!(shown(root.path(), Some(follows), Some(&there)), None);
        assert_eq!(shown(root.path(), None, Some(&checkout)), None);
        keep(root.path(), Kind::Failed, "no network").unwrap();
        for from in [Some(&there), Some(&checkout), None] {
            assert!(shown(root.path(), Some(follows), from).is_some());
            assert!(shown(root.path(), None, from).is_some());
        }
    }

    #[test]
    fn logins_show_a_failure_or_a_pause_while_it_holds_and_nothing_once_upgrades_work() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("wrath/system")).unwrap();
        let notice = || std::fs::read_to_string(root.path().join(NOTICE)).ok();
        let follows = "github:johnae/machines//framework";
        let there = ConfigFrom::Url {
            url: follows.into(),
            rev: None,
            date: None,
        };
        let checkout = ConfigFrom::Dir("/home/john/machines/framework".into());

        keep(root.path(), Kind::Failed, "no network").unwrap();
        let failed = notice().unwrap();
        assert!(
            failed.starts_with("wrath: the last automatic upgrade failed"),
            "{failed}"
        );
        assert!(failed.contains("no network"), "{failed}");
        keep(root.path(), Kind::Done, "up to date").unwrap();
        assert_eq!(notice(), None);

        keep(root.path(), Kind::Paused, "a checkout").unwrap();
        assert!(notice().unwrap().contains("paused"));
        applied_by_hand(root.path(), follows, Some(&checkout)).unwrap();
        assert!(notice().unwrap().contains("paused"));
        // Applied by hand from what it follows: up to date, so nothing to say, failed or paused.
        applied_by_hand(root.path(), follows, Some(&there)).unwrap();
        assert_eq!(notice(), None);
        keep(root.path(), Kind::Failed, "no network").unwrap();
        applied_by_hand(root.path(), follows, Some(&checkout)).unwrap();
        assert!(notice().unwrap().contains("no network"));
        applied_by_hand(root.path(), follows, Some(&there)).unwrap();
        assert_eq!(notice(), None);
        applied_by_hand(root.path(), follows, Some(&there)).unwrap();
    }

    #[test]
    fn waiting_for_a_cache_is_kept_for_status_and_logins_say_nothing_of_it() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("wrath/system")).unwrap();
        keep(root.path(), Kind::Failed, "no network").unwrap();
        keep(root.path(), Kind::Waiting, "it would build 1 (jq)").unwrap();
        assert!(!root.path().join(NOTICE).exists());
        let last = last(root.path()).unwrap();
        assert_eq!(last.kind, Kind::Waiting);
        assert!(
            last.to_string()
                .starts_with("the last automatic upgrade waited, ")
        );
    }

    #[test]
    fn automatic_upgrades_build_unless_the_configuration_says_not_to() {
        let mut model = Model::default();
        assert!(builds(&model));
        model
            .options
            .insert("updates.build".into(), wrath_eval::Value::Bool(false));
        assert!(!builds(&model));
        model
            .options
            .insert("updates.build".into(), wrath_eval::Value::Bool(true));
        assert!(builds(&model));
    }

    #[test]
    fn a_pause_keeps_when_it_started_and_anything_else_replaces_it() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("wrath/system")).unwrap();
        assert_eq!(last(root.path()), None);
        keep(root.path(), Kind::Paused, "a checkout").unwrap();
        let started = last(root.path()).unwrap();
        std::fs::write(
            root.path().join(OUTCOME),
            std::fs::read_to_string(root.path().join(OUTCOME))
                .unwrap()
                .replace(&started.at, "2026-01-01 00:00:00"),
        )
        .unwrap();
        keep(root.path(), Kind::Paused, "a checkout").unwrap();
        assert_eq!(last(root.path()).unwrap().at, "2026-01-01 00:00:00");
        keep(root.path(), Kind::Failed, "no network").unwrap();
        let failed = last(root.path()).unwrap();
        assert_eq!(
            (failed.kind, failed.message.as_str()),
            (Kind::Failed, "no network")
        );
        assert_ne!(failed.at, "2026-01-01 00:00:00");
    }
}
