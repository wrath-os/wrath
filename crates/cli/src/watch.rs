//! `apply --watch`: tries each save of the configuration on the machine. A save that only changes
//! settings is applied by itself; anything more shows the plan and asks first, as `apply` does.
//! Nothing becomes a generation until it's kept: stopping asks whether to keep what runs, and a
//! watcher that dies leaves a trial that switches back on its own.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use distro_model::{Model, Plan};

use crate::generations::{self, Switch};
use crate::host::Remote;
use crate::switching as here;

/// How long a trial lasts after the last save or beat, so a watcher that dies switches back.
const TRIAL: u64 = 600;
/// How often a watcher with nothing to apply starts the trial's clock over.
const BEAT: Duration = Duration::from_secs(TRIAL / 2);
/// How often the configuration is looked at for a save.
const POLL: Duration = Duration::from_millis(200);

/// The machine being watched for: this one, or another reached over SSH.
pub enum Machine<'a> {
    Here,
    Remote(&'a Remote),
}

impl Machine<'_> {
    /// The model of what the machine runs, a system being tried included.
    fn running(&self) -> Result<Option<Model>, String> {
        match self {
            Self::Here => Ok(here::running_model().and_then(|path| crate::read_model(&path).ok())),
            Self::Remote(remote) => remote.running_model(),
        }
    }

    /// Builds `model` here and tries it on the machine for [`TRIAL`] seconds.
    fn try_model(&self, root: &Path, model: &Model) -> Result<(), String> {
        let built = generations::build_generation(root, model, false)?;
        match self {
            Self::Here => here::try_system(&built, TRIAL),
            Self::Remote(remote) => remote.switch_built(root, &built, Switch::Try(TRIAL)),
        }
    }

    /// Makes what's being tried the current generation.
    fn keep(&self) -> Result<(), String> {
        match self {
            Self::Here => generations::keep().map(drop),
            Self::Remote(remote) => remote.distro_as_root("keep"),
        }
    }

    /// Switches back to the current generation.
    fn end(&self) -> Result<(), String> {
        match self {
            Self::Here => generations::rollback(None).map(drop),
            Self::Remote(remote) => remote.distro_as_root("rollback"),
        }
    }
}

/// What a watcher keeps between saves.
struct Watching<'a> {
    root: &'a Path,
    config: &'a Path,
    machine: Machine<'a>,
    /// The model the machine runs, as far as the watcher knows.
    running: Option<Model>,
    /// When the watcher last tried something, while a trial of its own runs.
    tried: Option<Instant>,
}

/// Watches `config`, evaluated by `evaluate` on each save, and tries what it describes on
/// `machine` until stopped with Ctrl-C.
pub fn watch(
    root: &Path,
    config: &Path,
    machine: Machine<'_>,
    evaluate: impl Fn() -> Option<Model>,
) -> Result<ExitCode, String> {
    let stopping = Arc::new(AtomicBool::new(false));
    let flag = stopping.clone();
    ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst))
        .map_err(|e| format!("cannot watch for Ctrl-C: {e}"))?;
    let mut watching = Watching {
        root,
        config,
        running: machine.running()?,
        machine,
        tried: None,
    };
    println!(
        "watching {}: a save that only changes settings is tried at once, anything more asks \
         first; Ctrl-C stops",
        config.display()
    );
    let mut seen = fingerprint(config);
    watching.save(&evaluate);
    while !stopping.load(Ordering::SeqCst) {
        std::thread::sleep(POLL);
        let now = fingerprint(config);
        if now != seen {
            seen = now;
            watching.save(&evaluate);
        } else if watching.tried.is_some_and(|at| at.elapsed() > BEAT) {
            watching.beat();
        }
    }
    watching.stop()
}

impl Watching<'_> {
    /// Evaluates the configuration as saved and tries it, by itself when it only changes
    /// settings and otherwise after asking. A save that fails anywhere waits for the next.
    fn save(&mut self, evaluate: &impl Fn() -> Option<Model>) {
        let Some(model) = evaluate() else {
            println!("waiting for the next save");
            return;
        };
        if let Err(message) = self.offer(model) {
            crate::style::error(&message);
            println!("waiting for the next save");
        }
    }

    fn offer(&mut self, model: Model) -> Result<(), String> {
        let model = generations::locked(self.root, self.config, model)?;
        let plan = distro_model::plan(self.running.as_ref().unwrap_or(&Model::default()), &model);
        if plan.is_empty() {
            println!("nothing to try: it runs this already");
            return Ok(());
        }
        let settings = self.running.is_some() && plan.settings_only();
        if !settings {
            crate::print_plan(&plan);
            if !generations::confirmed(false, "more than settings changed; try it?")? {
                println!("not tried");
                return Ok(());
            }
        }
        self.machine.try_model(self.root, &model)?;
        self.running = Some(model);
        self.tried = Some(Instant::now());
        if settings {
            let files: Vec<&str> = plan.files.iter().map(|file| file.name.as_str()).collect();
            println!("tried {}{}", files.join(", "), effect(&plan));
        } else {
            println!("tried it{}", effect(&plan));
        }
        Ok(())
    }

    /// Tries what runs again, so its trial doesn't end while the watcher is still going.
    fn beat(&mut self) {
        let Some(model) = &self.running else {
            return;
        };
        match self.machine.try_model(self.root, model) {
            Ok(()) => self.tried = Some(Instant::now()),
            Err(message) => crate::style::error(&message),
        }
    }

    /// Asks whether to keep what the watcher tried, and keeps it or switches back.
    fn stop(&self) -> Result<ExitCode, String> {
        println!();
        if self.tried.is_none() {
            println!("stopped watching");
            return Ok(ExitCode::SUCCESS);
        }
        if generations::confirmed(false, "keep what runs as a generation?")? {
            self.machine.keep()?;
        } else {
            self.machine.end()?;
        }
        Ok(ExitCode::SUCCESS)
    }
}

/// When what `plan` changes takes effect, when that's later than now.
fn effect(plan: &Plan) -> String {
    plan.tier()
        .and_then(generations::full_effect)
        .map(|note| format!("; {note}"))
        .unwrap_or_default()
}

/// What changes when a file in `config` is saved: each file's path, size and time, leaving out
/// the lock, which applying writes, and version control's directories.
fn fingerprint(config: &Path) -> Vec<(String, u64, Option<SystemTime>)> {
    let mut seen = Vec::new();
    let mut pending = vec![config.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                if name != ".jj" && name != ".git" {
                    pending.push(entry.path());
                }
            } else if name != crate::LOCK_FILE {
                let path = entry.path().to_string_lossy().into_owned();
                seen.push((path, meta.len(), meta.modified().ok()));
            }
        }
    }
    seen.sort();
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_save_changes_the_fingerprint_and_the_lock_and_history_dont() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path();
        std::fs::write(config.join("system.toml"), "[system]\n").unwrap();
        std::fs::create_dir_all(config.join(".jj/repo")).unwrap();
        let before = fingerprint(config);
        std::fs::write(config.join(crate::LOCK_FILE), "version = 4\n").unwrap();
        std::fs::write(config.join(".jj/repo/op"), "x").unwrap();
        assert_eq!(fingerprint(config), before);
        std::fs::create_dir_all(config.join("wezterm")).unwrap();
        std::fs::write(config.join("wezterm/wezterm.lua"), "return {}\n").unwrap();
        assert_ne!(fingerprint(config), before);
    }
}
