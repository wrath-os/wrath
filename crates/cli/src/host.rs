//! `--host NAME`: building a configuration here and switching another machine to it over SSH.
//! The machine needs SSH and root, through `sudo` when the login isn't root's, and nothing to
//! build with: what it lacks of the system is copied into its store first.

use std::collections::BTreeSet;
use std::io::IsTerminal;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use distro_build::store::{Lab, StorePath};
use distro_model::Model;

use crate::generations::{self, Switch};

/// Where a system keeps its model, on the machine it runs on.
const RUNNING_MODEL: &str = "/run/system/etc/distro/model.json";

/// A machine reached with `ssh NAME`, so `~/.ssh/config` and the agent apply. `DISTRO_SSH`
/// replaces `ssh`, with options, such as `ssh -p 2222`. The connections share one, so a password
/// or a new host key is asked for once.
pub struct Remote {
    name: String,
    ssh: Vec<String>,
    shared: tempfile::TempDir,
}

impl Remote {
    pub fn new(name: &str) -> Result<Self, String> {
        let ssh = std::env::var("DISTRO_SSH").unwrap_or_else(|_| "ssh".to_owned());
        Ok(Self {
            name: name.to_owned(),
            ssh: ssh.split_whitespace().map(str::to_owned).collect(),
            shared: tempfile::tempdir().map_err(|e| format!("cannot make a directory: {e}"))?,
        })
    }

    fn ssh(&self) -> Command {
        self.ssh_with(&[])
    }

    /// `ssh` with `options` before the machine's name.
    fn ssh_with(&self, options: &[&str]) -> Command {
        let mut command = Command::new(&self.ssh[0]);
        command
            .args(&self.ssh[1..])
            .args(["-o", "ControlMaster=auto", "-o", "ControlPersist=60", "-o"])
            .arg(format!("ControlPath={}/%C", self.shared.path().display()))
            .args(options)
            .arg("--")
            .arg(&self.name);
        command
    }

    /// What `script` prints on the machine, run by its login shell.
    fn output(&self, script: &str) -> Result<String, String> {
        let output = self
            .ssh()
            .arg(script)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run ssh: {e}"))?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        Err(format!(
            "{}: `{script}` failed: {}",
            self.name,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }

    /// The model the machine runs, or `None` when distro didn't build what it runs.
    pub fn running_model(&self) -> Result<Option<Model>, String> {
        let text = self.output(&format!("cat {RUNNING_MODEL} 2>/dev/null || true"))?;
        if text.trim().is_empty() {
            return Ok(None);
        }
        Model::from_json(&text)
            .map(Some)
            .map_err(|e| format!("{}: {RUNNING_MODEL}: {e}", self.name))
    }

    /// Copies what the machine lacks of `system`, built in `lab`, to a file on it, and returns
    /// that file, or `None` when it has everything already.
    fn send(&self, lab: &Lab, system: &StorePath) -> Result<Option<String>, String> {
        let have = self.output("ls /store 2>/dev/null || true")?;
        let closure = distro_build::chain::runtime_closure(lab, system)?;
        let missing = missing(&have, closure.iter().map(StorePath::hash));
        if missing.is_empty() {
            return Ok(None);
        }
        eprintln!("copying {} store paths to {}", missing.len(), self.name);
        let file = format!("/var/tmp/distro-{}.tar", system.hash());
        let mut tar = Command::new("tar")
            .args(["--owner=0", "--group=0", "--numeric-owner", "-C"])
            .arg(lab.work.join("store"))
            .arg("-cf")
            .arg("-")
            .args(&missing)
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run tar: {e}"))?;
        let stream = tar.stdout.take().expect("piped");
        let sent = self
            .ssh()
            .arg(format!("cat > {file}"))
            .stdin(stream)
            .status()
            .map_err(|e| format!("cannot run ssh: {e}"))?;
        let packed = tar.wait().map_err(|e| format!("tar: {e}"))?;
        if !packed.success() || !sent.success() {
            return Err(format!("copying the system to {} failed", self.name));
        }
        Ok(Some(file))
    }

    /// Puts what was sent into the machine's store and switches it to `system` as `how` says,
    /// with the system's own distro. Asks for a password when sudo wants one.
    fn switch(&self, sent: Option<&str>, system: &StorePath, how: Switch) -> Result<(), String> {
        let root = self.output("id -u")?.trim() == "0";
        let script = switch_script(sent, system, how, root);
        // A terminal, when there is one, so sudo on the machine can ask for a password.
        let terminal: &[&str] = if std::io::stdin().is_terminal() {
            &["-t"]
        } else {
            &[]
        };
        let status = self
            .ssh_with(terminal)
            .arg(script)
            .status()
            .map_err(|e| format!("cannot run ssh: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("switching {} failed", self.name))
        }
    }
}

/// The store paths of `closure` that aren't in `have`, as `ls /store` lists it.
fn missing<'a>(have: &str, closure: impl Iterator<Item = &'a str>) -> Vec<String> {
    let have: BTreeSet<&str> = have.lines().collect();
    closure
        .filter(|hash| !have.contains(hash))
        .map(str::to_owned)
        .collect()
}

/// The script that unpacks `sent` into the store and switches to `system`, through sudo unless
/// it runs as root.
fn switch_script(sent: Option<&str>, system: &StorePath, how: Switch, root: bool) -> String {
    let path = system.inside();
    let switch = match how {
        Switch::Apply => format!("{path}/bin/distro apply {path}"),
        Switch::Try(seconds) => format!("{path}/bin/distro try --for {seconds}s {path}"),
    };
    let steps = match sent {
        Some(file) => format!("tar -C /store -xf {file} && rm -f {file} && {switch}"),
        None => switch,
    };
    if root {
        steps
    } else {
        format!("sudo sh -c '{steps}'")
    }
}

/// Builds `model` here and switches the machine to it, showing the plan against what the
/// machine runs and asking first. The machine's requirements and its own values are checked
/// and filled in on it, by the system's own distro.
pub fn apply(
    root: &Path,
    config: &Path,
    model: Model,
    remote: &Remote,
    how: Switch,
    yes: bool,
) -> Result<ExitCode, String> {
    let model = generations::locked(root, config, model)?;
    let running = remote.running_model()?;
    if running.as_ref().is_some_and(|r| r.hash() == model.hash()) {
        println!("{} runs model {} already", remote.name, &model.hash()[..16]);
        return Ok(ExitCode::SUCCESS);
    }
    let plan = distro_model::plan(running.as_ref().unwrap_or(&Model::default()), &model);
    if running.is_some() {
        crate::print_plan(&plan);
    } else {
        println!(
            "distro didn't build what {} runs, so all of it is new: {}",
            remote.name,
            plan.summary(5)
        );
    }
    if !generations::confirmed(yes, &format!("switch {} to it?", remote.name))? {
        println!("nothing changed");
        return Ok(ExitCode::SUCCESS);
    }
    let built = generations::build_generation(root, &model, false)?;
    let name = built
        .file_name()
        .ok_or("a generation has a name")?
        .to_string_lossy()
        .into_owned();
    let system = StorePath::from_hash(&name);
    let lab = Lab::new(root.join("build/work"));
    let sent = remote.send(&lab, &system)?;
    remote.switch(sent.as_deref(), &system, how)?;
    if let Some(note) = running.and(plan.tier()).and_then(generations::full_effect) {
        println!("{note}");
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_what_the_machine_lacks_is_sent() {
        let have = "aaa\nccc\n.system\n";
        assert_eq!(
            missing(have, ["aaa", "bbb", "ccc", "ddd"].into_iter()),
            ["bbb", "ddd"]
        );
        assert!(missing(have, ["aaa"].into_iter()).is_empty());
    }

    #[test]
    fn the_machine_switches_with_the_new_systems_distro() {
        let system = StorePath::from_hash("abc");
        assert_eq!(
            switch_script(Some("/var/tmp/x.tar"), &system, Switch::Apply, true),
            "tar -C /store -xf /var/tmp/x.tar && rm -f /var/tmp/x.tar && \
             /store/abc/bin/distro apply /store/abc"
        );
        assert_eq!(
            switch_script(None, &system, Switch::Try(60), false),
            "sudo sh -c '/store/abc/bin/distro try --for 60s /store/abc'"
        );
    }
}
