//! `--host NAME`: building a configuration here and switching another machine to it over SSH.
//! The machine needs SSH and root, through `sudo` when the login isn't root's, and nothing to
//! build with: what it lacks of the system is copied into its store first.

use std::collections::BTreeSet;
use std::io::IsTerminal;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use distro_build::store::{Lab, StorePath, is_entry};
use distro_model::Model;

use crate::activate::{CURRENT, UNFINISHED, Unfinished};
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
        let ssh = std::env::var("DISTRO_SSH")
            .ok()
            .filter(|ssh| !ssh.trim().is_empty())
            .unwrap_or_else(|| "ssh".to_owned());
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

    /// What the machine recorded about itself when it was installed, such as its role.
    pub fn given(&self) -> Result<crate::machine::Given, String> {
        let file = crate::machine::MACHINE_FILE;
        let text = self.output(&format!("cat {file} 2>/dev/null || true"))?;
        crate::machine::Given::parse(&text, &format!("{}:{file}", self.name))
    }

    /// The machine's name as `ssh` is given it, without a user or port: `root@framework` is
    /// `framework`.
    pub fn host_name(&self) -> &str {
        let host = self.name.rsplit('@').next().unwrap_or(&self.name);
        host.split(':').next().unwrap_or(host)
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

    /// The switch to what the machine runs that failed partway, with the model it started from,
    /// when there is one.
    fn unfinished(&self) -> Result<Option<(Unfinished, Option<Model>)>, String> {
        let text = self.output(&format!("cat /{UNFINISHED} 2>/dev/null || true"))?;
        if text.trim().is_empty() {
            return Ok(None);
        }
        let running = self.output("readlink -f /run/system")?;
        let Some(unfinished) = Unfinished::parse(&text, Path::new(running.trim())) else {
            return Ok(None);
        };
        // Only a store path goes into the command.
        let entry = unfinished
            .from
            .as_deref()
            .and_then(|from| from.strip_prefix("/store").ok());
        let from = match entry.and_then(Path::to_str).filter(|name| is_entry(name)) {
            Some(from) => {
                let path = format!("/store/{from}/etc/distro/model.json");
                let text = self.output(&format!("cat {path} 2>/dev/null || true"))?;
                Model::from_json(&text).ok()
            }
            None => None,
        };
        Ok(Some((unfinished, from)))
    }

    /// What the system the machine runs says it was built from, when it says.
    pub fn running_built_from(&self) -> Result<Option<String>, String> {
        let keys = distro_build::graph::PACKAGE_KEYS;
        let text = self.output(&format!("cat /run/system/{keys} 2>/dev/null || true"))?;
        Ok((!text.trim().is_empty()).then_some(text))
    }

    /// Copies what the machine lacks of `system`, built in `lab`, and `config`, the configuration
    /// it's built from, into a new directory on it. `None` when there's nothing to copy.
    fn send(
        &self,
        lab: &Lab,
        system: &StorePath,
        config: Option<&Path>,
    ) -> Result<Option<Sent>, String> {
        let have = self.output("ls /store 2>/dev/null || true")?;
        let closure = distro_build::chain::runtime_closure(lab, system)?;
        let missing = missing(&have, closure.iter().map(StorePath::hash));
        if missing.is_empty() && config.is_none() {
            return Ok(None);
        }
        // A directory only this user can write to, so nobody else on the machine can swap an
        // archive before root unpacks it.
        let dir = sent_dir(&self.output("mktemp -d /var/tmp/distro.XXXXXX")?)?;
        let owned_by_root = ["--owner=0", "--group=0", "--numeric-owner"];
        if !missing.is_empty() {
            eprintln!("copying {} store paths to {}", missing.len(), self.name);
            let mut tar = Command::new("tar");
            tar.args(owned_by_root)
                .arg("-C")
                .arg(lab.work.join("store"))
                .args(["-cf", "-"])
                .args(&missing);
            self.upload(tar, &format!("{dir}/system.tar"))?;
        }
        if let Some(config) = config {
            let mut tar = Command::new("tar");
            tar.args(owned_by_root)
                .args(["--exclude=./.git", "--exclude=./.jj", "-C"])
                .arg(config)
                .args(["-cf", "-", "."]);
            self.upload(tar, &format!("{dir}/config.tar"))?;
        }
        Ok(Some(Sent {
            dir,
            system: !missing.is_empty(),
            config: config.is_some(),
        }))
    }

    /// Writes what `tar` prints to `path` on the machine.
    fn upload(&self, mut tar: Command, path: &str) -> Result<(), String> {
        let mut tar = tar
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run tar: {e}"))?;
        let stream = tar.stdout.take().expect("piped");
        let sent = self
            .ssh()
            .arg(format!("cat > {path}"))
            .stdin(stream)
            .status()
            .map_err(|e| format!("cannot run ssh: {e}"))?;
        let packed = tar.wait().map_err(|e| format!("tar: {e}"))?;
        if !packed.success() || !sent.success() {
            return Err(format!("copying {path} to {} failed", self.name));
        }
        Ok(())
    }

    /// Puts what was sent into the machine's store and switches it to `system` as `how` says,
    /// with the system's own distro. Asks for a password when sudo wants one.
    fn switch(&self, sent: Option<&Sent>, system: &StorePath, how: Switch) -> Result<(), String> {
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

/// What `send` copied to the machine, in the directory it made there.
#[derive(Debug)]
struct Sent {
    dir: String,
    /// `system.tar`, the store paths the machine lacked.
    system: bool,
    /// `config.tar`, the configuration applied.
    config: bool,
}

/// The script that unpacks what was `sent` into the store and switches to `system`, through sudo
/// unless it runs as root. The configuration sent replaces the one in /etc/distro once `system`
/// is the current generation, even when the switch failed partway, so the machine's copy always
/// describes what it boots.
fn switch_script(sent: Option<&Sent>, system: &StorePath, how: Switch, root: bool) -> String {
    let path = system.inside();
    let switch = match how {
        Switch::Apply => format!("{path}/bin/distro apply --yes {path}"),
        Switch::Try(seconds) => format!("{path}/bin/distro try --yes --for {seconds}s {path}"),
    };
    let mut steps = Vec::new();
    // Unpacked beside the store and renamed in one path at a time, so an interrupted copy
    // never leaves a partial path under a name `send` would take as complete.
    if let Some(Sent { dir, .. }) = sent.filter(|sent| sent.system) {
        steps.push(format!(
            "staged=$(mktemp -d /store/.incoming.XXXXXX) && tar -C $staged -xf {dir}/system.tar && \
             for path in $staged/*; do test -e /store/${{path##*/}} || mv $path /store/ || exit; \
             done && rm -rf $staged || exit"
        ));
    }
    steps.push(format!("{switch}; status=$?"));
    if let Some(Sent { dir, .. }) = sent.filter(|sent| sent.config) {
        // What the system put there, its model and such, are links, and stay.
        steps.push(format!(
            "if [ \"$(readlink -f /{CURRENT})\" = {path} ]; then mkdir -p /etc/distro && \
             find /etc/distro -mindepth 1 -maxdepth 1 ! -type l -exec rm -rf {{}} + && \
             tar -C /etc/distro --no-same-owner -xf {dir}/config.tar; fi"
        ));
    }
    if let Some(sent) = sent {
        steps.push(format!("rm -rf {}", sent.dir));
    }
    steps.push("exit $status".to_owned());
    let steps = steps.join("; ");
    if root {
        format!("sh -c '{steps}'")
    } else {
        format!("sudo sh -c '{steps}'")
    }
}

/// The directory `send` made on the machine, from what it printed.
fn sent_dir(printed: &str) -> Result<String, String> {
    let dir = printed.trim_end();
    let suffix = dir.strip_prefix("/var/tmp/distro.").unwrap_or_default();
    if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(format!(
            "the machine made an unexpected directory for the copy: {dir:?}"
        ));
    }
    Ok(dir.to_owned())
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
    // After a switch that failed partway, the plan is from where it started, so switching again
    // does all it didn't.
    let (running, unfinished) = match remote.unfinished()? {
        Some((unfinished, from)) => {
            println!("{}", generations::unfinished_note(&unfinished.error));
            (from, true)
        }
        None => (remote.running_model()?, false),
    };
    let rebuilt = generations::rebuilt(root, &model, remote.running_built_from()?.as_deref());
    let same = running.as_ref().is_some_and(|r| r.hash() == model.hash());
    if same && !unfinished && rebuilt.as_ref().is_some_and(Vec::is_empty) {
        println!("{} runs model {} already", remote.name, &model.hash()[..16]);
        return Ok(ExitCode::SUCCESS);
    }
    let plan = distro_model::plan(running.as_ref().unwrap_or(&Model::default()), &model);
    let plan = generations::with_rebuilt(plan, rebuilt);
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
    // A system only tried isn't what the machine boots, so its configuration stays here.
    let kept = matches!(how, Switch::Apply).then_some(config);
    let sent = remote.send(&lab, &system, kept)?;
    remote.switch(sent.as_ref(), &system, how)?;
    if let Some(note) = running.and(plan.tier()).and_then(generations::full_effect) {
        println!("{note}");
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_is_named_without_its_user_or_port() {
        for (given, name) in [
            ("root@framework", "framework"),
            ("framework", "framework"),
            ("me@w-01:2222", "w-01"),
        ] {
            assert_eq!(Remote::new(given).unwrap().host_name(), name);
        }
    }

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
        let sent = Sent {
            dir: "/var/tmp/distro.Ab12".into(),
            system: true,
            config: true,
        };
        let script = switch_script(Some(&sent), &system, Switch::Apply, true);
        let inner = script
            .strip_prefix("sh -c '")
            .and_then(|s| s.strip_suffix('\''))
            .unwrap();
        let checked = Command::new("sh")
            .args(["-n", "-c", inner])
            .status()
            .unwrap();
        assert!(checked.success(), "{inner}");
        let at = |needle: &str| {
            inner
                .find(needle)
                .unwrap_or_else(|| panic!("{needle}: {inner}"))
        };
        let unpack = at("tar -C $staged -xf /var/tmp/distro.Ab12/system.tar");
        let switch = at("/store/abc/bin/distro apply --yes /store/abc; status=$?");
        let keep = at("if [ \"$(readlink -f /store/.system/current)\" = /store/abc ]");
        let config = at("tar -C /etc/distro --no-same-owner -xf /var/tmp/distro.Ab12/config.tar");
        assert!(unpack < switch && switch < keep && keep < config, "{inner}");
        assert!(
            inner.ends_with("rm -rf /var/tmp/distro.Ab12; exit $status"),
            "{inner}"
        );

        assert_eq!(
            switch_script(None, &system, Switch::Try(60), false),
            "sudo sh -c '/store/abc/bin/distro try --yes --for 60s /store/abc; status=$?; \
             exit $status'"
        );
    }

    #[test]
    fn only_a_fresh_directory_from_mktemp_is_taken_as_what_was_sent() {
        assert_eq!(
            sent_dir("/var/tmp/distro.Ab12\n"),
            Ok("/var/tmp/distro.Ab12".to_owned())
        );
        for printed in [
            "",
            "/var/tmp/distro.",
            "/var/tmp/distro.a b",
            "/tmp/x",
            "/var/tmp/distro.a;rm",
        ] {
            assert!(sent_dir(printed).is_err(), "{printed:?}");
        }
    }
}
