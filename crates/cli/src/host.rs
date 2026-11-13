//! `--host NAME`: building a configuration here and switching another machine to it over SSH.
//! The machine needs SSH and root, through `sudo` when the login isn't root's, and nothing to
//! build with: what it lacks of the system is copied into its store first.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitCode, Stdio};

use wrath_build::store::{Lab, StorePath, is_entry};
use wrath_model::Model;

use crate::activate::{UNFINISHED, Unfinished};
use crate::generations::{self, Switch};

/// Where a system keeps its model, on the machine it runs on.
const RUNNING_MODEL: &str = "/run/system/etc/wrath/model.json";

/// A machine reached with `ssh NAME`, so `~/.ssh/config` and the agent apply. `WRATH_SSH`
/// replaces `ssh`, with options, such as `ssh -p 2222`. The connections share one, so a password
/// or a new host key is asked for once.
pub struct Remote {
    name: String,
    ssh: Vec<String>,
    shared: tempfile::TempDir,
    /// Leaves out what copying and switching say as they go, as watching does, which says one
    /// line of its own.
    quiet: bool,
    /// A root shell on the machine that root commands go through, once [`Remote::hold_root`]
    /// opened it.
    held: RefCell<Option<RootShell>>,
}

impl Remote {
    pub fn new(name: &str) -> Result<Self, String> {
        let ssh = std::env::var("WRATH_SSH")
            .ok()
            .filter(|ssh| !ssh.trim().is_empty())
            .unwrap_or_else(|| "ssh".to_owned());
        Ok(Self {
            name: name.to_owned(),
            ssh: ssh.split_whitespace().map(str::to_owned).collect(),
            shared: tempfile::tempdir().map_err(|e| format!("cannot make a directory: {e}"))?,
            quiet: false,
            held: RefCell::new(None),
        })
    }

    /// This machine, said less about: see [`Remote::quiet`].
    pub fn quietly(mut self) -> Self {
        self.quiet = true;
        self
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

    /// The model the machine runs, or `None` when wrath didn't build what it runs.
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
                let path = format!("/store/{from}/etc/wrath/model.json");
                let text = self.output(&format!("cat {path} 2>/dev/null || true"))?;
                Model::from_json(&text).ok()
            }
            None => None,
        };
        Ok(Some((unfinished, from)))
    }

    /// What the system the machine runs says it was built from, when it says.
    pub fn running_built_from(&self) -> Result<Option<String>, String> {
        let keys = wrath_build::graph::PACKAGE_KEYS;
        let text = self.output(&format!("cat /run/system/{keys} 2>/dev/null || true"))?;
        Ok((!text.trim().is_empty()).then_some(text))
    }

    /// Copies what the machine lacks of `system`, built in `lab`, into a new directory on it,
    /// and returns that directory, or `None` when it has everything already.
    fn send(&self, lab: &Lab, system: &StorePath) -> Result<Option<String>, String> {
        let have = self.output("ls /store 2>/dev/null || true")?;
        let closure = wrath_build::chain::runtime_closure(lab, system)?;
        let missing = missing(&have, closure.iter().map(StorePath::hash));
        if missing.is_empty() {
            return Ok(None);
        }
        if !self.quiet {
            eprintln!("copying {} store paths to {}", missing.len(), self.name);
        }
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
        // A directory only this user can write to, so nobody else on the machine can swap the
        // archive before root unpacks it.
        let sent = self
            .ssh()
            .arg(
                "sh -c 'dir=$(mktemp -d /var/tmp/wrath.XXXXXX) && \
                 cat > $dir/system.tar && echo $dir'",
            )
            .stdin(stream)
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| format!("cannot run ssh: {e}"))?;
        let packed = tar.wait().map_err(|e| format!("tar: {e}"))?;
        if !packed.success() || !sent.status.success() {
            return Err(format!("copying the system to {} failed", self.name));
        }
        sent_dir(&String::from_utf8_lossy(&sent.stdout)).map(Some)
    }

    /// Puts what was sent into the machine's store and switches it to `system` as `how` says,
    /// with the system's own wrath. Asks for a password when sudo wants one.
    fn switch(&self, sent: Option<&str>, system: &StorePath, how: Switch) -> Result<(), String> {
        if self.run_as_root(&switch_steps(sent, system, how))? {
            Ok(())
        } else {
            Err(format!("switching {} failed", self.name))
        }
    }

    /// Copies `built`, a system built here, to the machine and switches it as `how` says.
    pub fn switch_built(&self, root: &Path, built: &Path, how: Switch) -> Result<(), String> {
        let name = built
            .file_name()
            .ok_or("a generation has a name")?
            .to_string_lossy()
            .into_owned();
        let system = StorePath::from_hash(&name);
        let lab = Lab::new(root.join("build/work"));
        let sent = self.send(&lab, &system)?;
        self.switch(sent.as_deref(), &system, how)
    }

    /// Runs the running system's `wrath` with `args` on the machine as root, as `keep` and
    /// `rollback`.
    pub fn wrath_as_root(&self, args: &str) -> Result<(), String> {
        if self.run_as_root(&format!("/run/system/bin/wrath {args}"))? {
            Ok(())
        } else {
            Err(format!("`wrath {args}` on {} failed", self.name))
        }
    }

    /// Keeps a root shell open on the machine for what's run as root from now on, so sudo asks
    /// for a password once, here, rather than for each command.
    pub fn hold_root(&self) -> Result<(), String> {
        let (shell, password) = if self.output("id -u")?.trim() == "0" {
            ("sh", None)
        } else if self
            .ssh()
            .arg("sudo -n true")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            ("sudo -n sh", None)
        } else {
            let password = read_password(&format!("sudo password on {}: ", self.host_name()))?;
            // Checked first: a wrong password given to the shell's sudo would have it read the
            // commands that follow as more tries.
            if !self.sudo_accepts(&password)? {
                return Err(format!("sudo on {} didn't take the password", self.name));
            }
            ("sudo -S -p '' sh", Some(password))
        };
        let ssh = self.ssh_with(&["-o", "LogLevel=ERROR"]);
        let shell = RootShell::open(ssh, shell, password.as_deref())?;
        *self.held.borrow_mut() = Some(shell);
        Ok(())
    }

    /// Whether sudo on the machine takes `password`.
    fn sudo_accepts(&self, password: &str) -> Result<bool, String> {
        let mut child = self
            .ssh_with(&["-o", "LogLevel=ERROR"])
            .arg("sudo -S -p '' -k true")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot run ssh: {e}"))?;
        let mut stdin = child.stdin.take().expect("piped");
        let _ = writeln!(stdin, "{password}");
        drop(stdin);
        let status = child.wait().map_err(|e| format!("ssh: {e}"))?;
        Ok(status.success())
    }

    /// Runs `steps` as root on the machine: in the held root shell, or else over SSH with a
    /// terminal, when there is one, so sudo can ask for a password. Whether they succeeded.
    fn run_as_root(&self, steps: &str) -> Result<bool, String> {
        if let Some(shell) = self.held.borrow_mut().as_mut() {
            return shell.run(steps, self.quiet);
        }
        let root = self.output("id -u")?.trim() == "0";
        // Only errors from ssh itself, not that the shared connection closed.
        let options: &[&str] = if std::io::stdin().is_terminal() {
            &["-t", "-o", "LogLevel=ERROR"]
        } else {
            &["-o", "LogLevel=ERROR"]
        };
        let mut command = self.ssh_with(options);
        if self.quiet {
            command.stdout(Stdio::null());
        }
        let status = command
            .arg(as_root(steps, root))
            .status()
            .map_err(|e| format!("cannot run ssh: {e}"))?;
        Ok(status.success())
    }
}

/// A root shell on the machine, held open over SSH. Each command it runs ends with a line of
/// its own that says how it went, which is how its end is told from what it prints.
struct RootShell {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// What the shell prints after each command, before its exit status.
const DONE: &str = "\u{1e}wrath-done ";

impl RootShell {
    /// Starts `shell` over `ssh`, giving `password` to sudo first when there is one.
    fn open(mut ssh: Command, shell: &str, password: Option<&str>) -> Result<Self, String> {
        let mut child = ssh
            .arg(shell)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run ssh: {e}"))?;
        let mut stdin = child.stdin.take().expect("piped");
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        if let Some(password) = password {
            writeln!(stdin, "{password}").map_err(|e| format!("ssh: {e}"))?;
        }
        let mut shell = Self {
            child,
            stdin,
            stdout,
        };
        if !shell.run("true", true)? {
            return Err("the root shell on the machine didn't start".into());
        }
        Ok(shell)
    }

    /// Runs `steps`, printing what they print unless `quiet`. Whether they succeeded.
    fn run(&mut self, steps: &str, quiet: bool) -> Result<bool, String> {
        let closed = || "the root shell on the machine closed".to_owned();
        writeln!(self.stdin, "( {steps} ); printf '{DONE}%s\\n' $?").map_err(|_| closed())?;
        self.stdin.flush().map_err(|_| closed())?;
        let mut line = String::new();
        loop {
            line.clear();
            if self.stdout.read_line(&mut line).map_err(|_| closed())? == 0 {
                return Err(closed());
            }
            if let Some(status) = line.trim_end().strip_prefix(DONE) {
                return Ok(status == "0");
            }
            if !quiet {
                print!("{line}");
            }
        }
    }
}

impl Drop for RootShell {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "exit");
        let _ = self.child.wait();
    }
}

/// Reads a password from the terminal without showing it.
pub(crate) fn read_password(prompt: &str) -> Result<String, String> {
    use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|e| format!("a password needs a terminal: {e}"))?;
    let shown = tcgetattr(&tty).map_err(|e| format!("terminal: {e}"))?;
    let mut hidden = shown.clone();
    hidden.local_modes.remove(LocalModes::ECHO);
    let mut out = &tty;
    let _ = write!(out, "{prompt}");
    let _ = out.flush();
    tcsetattr(&tty, OptionalActions::Now, &hidden).map_err(|e| format!("terminal: {e}"))?;
    let mut password = String::new();
    let read = BufReader::new(&tty).read_line(&mut password);
    let _ = tcsetattr(&tty, OptionalActions::Now, &shown);
    let _ = writeln!(out);
    read.map_err(|e| format!("terminal: {e}"))?;
    Ok(password.trim_end_matches(['\n', '\r']).to_owned())
}

/// The store paths of `closure` that aren't in `have`, as `ls /store` lists it.
fn missing<'a>(have: &str, closure: impl Iterator<Item = &'a str>) -> Vec<String> {
    let have: BTreeSet<&str> = have.lines().collect();
    closure
        .filter(|hash| !have.contains(hash))
        .map(str::to_owned)
        .collect()
}

/// The steps, run as root, that unpack `sent` into the store and switch to `system`.
fn switch_steps(sent: Option<&str>, system: &StorePath, how: Switch) -> String {
    let path = system.inside();
    let switch = match how {
        Switch::Apply => format!("{path}/bin/wrath apply --yes {path}"),
        Switch::Try(seconds) => format!("{path}/bin/wrath try --yes --for {seconds}s {path}"),
    };
    // Unpacked beside the store and renamed in one path at a time, so an interrupted copy
    // never leaves a partial path under a name `send` would take as complete.
    match sent {
        Some(dir) => format!(
            "staged=$(mktemp -d /store/.incoming.XXXXXX) && tar -C $staged -xf {dir}/system.tar && \
             for path in $staged/*; do test -e /store/${{path##*/}} || mv $path /store/ || exit; \
             done && rm -rf $staged {dir} && {switch}"
        ),
        None => switch,
    }
}

/// `steps` as one command that runs them as root, through sudo unless `root` already.
fn as_root(steps: &str, root: bool) -> String {
    if root {
        format!("sh -c '{steps}'")
    } else {
        format!("sudo sh -c '{steps}'")
    }
}

/// The directory `send` made on the machine, from what it printed.
fn sent_dir(printed: &str) -> Result<String, String> {
    let dir = printed.trim_end();
    let suffix = dir.strip_prefix("/var/tmp/wrath.").unwrap_or_default();
    if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(format!(
            "the machine made an unexpected directory for the copy: {dir:?}"
        ));
    }
    Ok(dir.to_owned())
}

/// Builds `model` here and switches the machine to it, showing the plan against what the
/// machine runs and asking first. The machine's requirements and its own values are checked
/// and filled in on it, by the system's own wrath.
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
    let rebuilt = generations::rebuilt(&model, remote.running_built_from()?.as_deref());
    let same = running.as_ref().is_some_and(|r| r.hash() == model.hash());
    if same && !unfinished && rebuilt.as_ref().is_some_and(Vec::is_empty) {
        println!("{} runs model {} already", remote.name, &model.hash()[..16]);
        return Ok(ExitCode::SUCCESS);
    }
    let plan = wrath_model::plan(running.as_ref().unwrap_or(&Model::default()), &model);
    let plan = generations::with_rebuilt(plan, rebuilt);
    if running.is_some() {
        crate::print_plan(&plan);
    } else {
        println!(
            "wrath didn't build what {} runs, so all of it is new: {}",
            remote.name,
            plan.summary(5)
        );
    }
    if !generations::confirmed(yes, &format!("switch {} to it?", remote.name))? {
        println!("nothing changed");
        return Ok(ExitCode::SUCCESS);
    }
    let built = generations::build_generation(&model, false)?;
    remote.switch_built(root, &built, how)?;
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
    fn the_machine_switches_with_the_new_systems_wrath() {
        let system = StorePath::from_hash("abc");
        assert_eq!(
            as_root(
                &switch_steps(Some("/var/tmp/wrath.Ab12"), &system, Switch::Apply),
                true
            ),
            "sh -c 'staged=$(mktemp -d /store/.incoming.XXXXXX) && \
             tar -C $staged -xf /var/tmp/wrath.Ab12/system.tar && \
             for path in $staged/*; do test -e /store/${path##*/} || mv $path /store/ || exit; \
             done && rm -rf $staged /var/tmp/wrath.Ab12 && \
             /store/abc/bin/wrath apply --yes /store/abc'"
        );
        assert_eq!(
            as_root(&switch_steps(None, &system, Switch::Try(60)), false),
            "sudo sh -c '/store/abc/bin/wrath try --yes --for 60s /store/abc'"
        );
    }

    #[test]
    fn a_held_shell_runs_one_command_after_another_and_says_how_each_went() {
        let mut sh = Command::new("sh");
        sh.arg("-c");
        let mut shell = RootShell::open(sh, "sh", None).unwrap();
        assert_eq!(shell.run("echo noise; true", true), Ok(true));
        assert_eq!(shell.run("false", true), Ok(false));
        assert_eq!(shell.run("exit 3", true), Ok(false));
        assert_eq!(shell.run("printf '%s\\n' still here", true), Ok(true));
    }

    #[test]
    fn only_a_fresh_directory_from_mktemp_is_taken_as_what_was_sent() {
        assert_eq!(
            sent_dir("/var/tmp/wrath.Ab12\n"),
            Ok("/var/tmp/wrath.Ab12".to_owned())
        );
        for printed in [
            "",
            "/var/tmp/wrath.",
            "/var/tmp/wrath.a b",
            "/tmp/x",
            "/var/tmp/wrath.a;rm",
        ] {
            assert!(sent_dir(printed).is_err(), "{printed:?}");
        }
    }
}
