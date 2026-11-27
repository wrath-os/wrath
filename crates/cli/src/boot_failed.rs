//! A generation the boot loader gave up on. The current generation's entry ran out of tries and
//! an older generation booted instead. Until something makes the running generation current,
//! that's read from the boot entries. `wrath rollback` to the generation that runs then keeps a
//! record of it, for `status` and for logins, until a new generation is made or the failed one is
//! rolled back to.

use std::path::Path;

use serde_json::json;

use crate::switching::Generation;

/// Where the machine keeps the record of the last generation that didn't boot.
const RECORD: &str = "wrath/system/boot-failed.json";

/// Where the boot loader's entries are, under the machine's root.
const ENTRIES: &str = "boot/loader/entries";

/// The subject of what logins show about it.
const NOTICE: &str = "boot";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootFailed {
    /// The generation that didn't boot.
    pub generation: u32,
    /// How many times the boot loader tried it.
    pub tries: u32,
    /// The generation that booted instead.
    pub runs: u32,
    /// When the running generation was made current, as `2026-11-27 03:12:09` in UTC; `None`
    /// while the failed one is still current.
    pub at: Option<String>,
}

impl BootFailed {
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "generation": self.generation,
            "tries": self.tries,
            "runs": self.runs,
            "at": self.at,
        })
    }

    fn from_json(value: &serde_json::Value) -> Option<Self> {
        let number = |key: &str| value[key].as_u64().and_then(|n| u32::try_from(n).ok());
        Some(Self {
            generation: number("generation")?,
            tries: number("tries")?,
            runs: number("runs")?,
            at: value["at"].as_str().map(str::to_owned),
        })
    }
}

impl std::fmt::Display for BootFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            generation,
            tries,
            runs,
            ..
        } = self;
        let times = if *tries == 1 { "try" } else { "tries" };
        write!(
            f,
            "generation {generation} didn't boot after {tries} {times}, so generation {runs} runs"
        )?;
        match &self.at {
            Some(at) => write!(
                f,
                " and was made current, {at}; `journalctl --list-boots` finds the failed boots"
            ),
            None => write!(
                f,
                "; `wrath rollback {runs}` makes it current, and applying the configuration again \
                 gives {generation} fresh tries"
            ),
        }
    }
}

/// The failed boot the machine at `root`, with `generations`, is in now: its current generation
/// ran out of tries and another one runs.
pub fn now(root: &Path, generations: &[Generation]) -> Option<BootFailed> {
    let current = generations.iter().find(|g| g.current && !g.in_use)?;
    let runs = generations.iter().rev().find(|g| g.in_use)?;
    let tries = crate::activate::gave_up_after(&root.join(ENTRIES), current.number)?;
    Some(BootFailed {
        generation: current.number,
        tries,
        runs: runs.number,
        at: None,
    })
}

/// The failed boot the machine at `root` is in, or the last one it recorded.
pub fn get(root: &Path, generations: &[Generation]) -> Option<BootFailed> {
    now(root, generations).or_else(|| recorded(root))
}

fn recorded(root: &Path) -> Option<BootFailed> {
    let text = std::fs::read_to_string(root.join(RECORD)).ok()?;
    BootFailed::from_json(&serde_json::from_str(&text).ok()?)
}

/// Keeps a record of `failed`, now that generation `runs` was made current `at`, and says so as
/// people log in.
pub fn record(root: &Path, failed: BootFailed, runs: u32, at: String) -> Result<(), String> {
    let failed = BootFailed {
        runs,
        at: Some(at),
        ..failed
    };
    let path = root.join(RECORD);
    std::fs::write(&path, failed.to_json().to_string() + "\n")
        .map_err(|e| format!("{}: {e}", path.display()))?;
    crate::notice::set(root, NOTICE, Some(&failed.to_string()))
}

/// Forgets the recorded failed boot once generation `number` is current, if that's a new one or
/// the one that failed.
pub fn forget_for(root: &Path, number: u32) -> Result<(), String> {
    let Some(failed) = recorded(root) else {
        return Ok(());
    };
    if number <= failed.runs {
        return Ok(());
    }
    let path = root.join(RECORD);
    crate::notice::remove(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    crate::notice::set(root, NOTICE, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generation(number: u32, current: bool, in_use: bool) -> Generation {
        Generation {
            number,
            created: String::new(),
            current,
            in_use,
            running: in_use,
            model_hash: None,
            from: None,
        }
    }

    fn machine(entries: &[&str]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(ENTRIES)).unwrap();
        std::fs::create_dir_all(root.path().join("wrath/system")).unwrap();
        for entry in entries {
            std::fs::write(root.path().join(ENTRIES).join(entry), "").unwrap();
        }
        root
    }

    #[test]
    fn a_current_generation_out_of_tries_that_doesnt_run_failed_to_boot() {
        let root = machine(&["wrath-4.conf", "wrath-5+0-3.conf"]);
        let generations = [generation(4, false, true), generation(5, true, false)];
        let failed = now(root.path(), &generations).unwrap();
        assert_eq!((failed.generation, failed.tries, failed.runs), (5, 3, 4));
        assert_eq!(
            failed.to_string(),
            "generation 5 didn't boot after 3 tries, so generation 4 runs; `wrath rollback 4` \
             makes it current, and applying the configuration again gives 5 fresh tries"
        );
    }

    #[test]
    fn a_generation_waiting_for_its_boot_or_with_tries_left_didnt_fail() {
        // Made current with `apply --boot`: it has its tries, and boots next.
        let root = machine(&["wrath-4.conf", "wrath-5+3.conf"]);
        let waiting = [generation(4, false, true), generation(5, true, false)];
        assert_eq!(now(root.path(), &waiting), None);
        // Tried once, and the machine was turned off and on into the older one by hand.
        let root = machine(&["wrath-4.conf", "wrath-5+2-1.conf"]);
        assert_eq!(now(root.path(), &waiting), None);
        // Out of tries, but it runs: a person chose it in the boot menu.
        let root = machine(&["wrath-4.conf", "wrath-5+0-3.conf"]);
        let chosen = [generation(4, false, false), generation(5, true, true)];
        assert_eq!(now(root.path(), &chosen), None);
        // No boot entries at all, as in a VM booted directly.
        let root = tempfile::tempdir().unwrap();
        assert_eq!(now(root.path(), &waiting), None);
    }

    #[test]
    fn a_recorded_failure_is_shown_until_a_newer_generation_is_current() {
        let root = machine(&["wrath-4.conf", "wrath-5+0-3.conf"]);
        let generations = [generation(4, false, true), generation(5, true, false)];
        let failed = now(root.path(), &generations).unwrap();
        record(root.path(), failed, 4, "2026-11-27 03:12:09".into()).unwrap();

        // Rolled back: 4 is current, and the record says what happened.
        let after = [generation(4, true, true), generation(5, false, false)];
        let kept = get(root.path(), &after).unwrap();
        assert_eq!(kept.at.as_deref(), Some("2026-11-27 03:12:09"));
        assert_eq!(
            kept.to_string(),
            "generation 5 didn't boot after 3 tries, so generation 4 runs and was made current, \
             2026-11-27 03:12:09; `journalctl --list-boots` finds the failed boots"
        );
        let notice = crate::notice::get(root.path(), NOTICE).unwrap();
        assert!(
            notice.starts_with("wrath: generation 5 didn't boot"),
            "{notice}"
        );

        // Rolling back further, or to the one that runs, keeps it.
        forget_for(root.path(), 3).unwrap();
        forget_for(root.path(), 4).unwrap();
        assert!(recorded(root.path()).is_some());
        // The failed one again, or a new one, forgets it.
        forget_for(root.path(), 5).unwrap();
        assert_eq!(recorded(root.path()), None);
        assert_eq!(crate::notice::get(root.path(), NOTICE), None);
        forget_for(root.path(), 6).unwrap();
    }
}
