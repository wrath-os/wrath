//! Running scenarios in QEMU virtual machines.
//!
//! A scenario names a test image built from `factory/nix`. The image output contains
//! `disk.qcow2`, a UEFI-bootable disk whose system runs the guest agent. Each run gets its own
//! directory with a copy-on-write overlay of the disk, writable UEFI variables, an optional
//! software TPM, logs, and a script that boots the same VM interactively.

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use distro_agent::{Client, Request, Response};
use serde::Deserialize;
use vsock::VsockStream;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmSpec {
    /// Flake reference to a test image, relative to the repository root.
    pub image: String,
    #[serde(default)]
    pub firmware: Firmware,
    #[serde(default)]
    pub tpm: bool,
    /// Conditions to wait for, in order, before the expectations run.
    #[serde(default)]
    pub wait_for: Vec<WaitFor>,
    #[serde(default)]
    pub expect: Vec<Expect>,
    /// Limit for booting and checking. Building the image is not counted.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_timeout_secs() -> u64 {
    300
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Firmware {
    #[default]
    Uefi,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub enum WaitFor {
    /// The guest agent answers.
    Agent,
    /// A systemd unit is active.
    Unit(String),
}

impl TryFrom<String> for WaitFor {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value == "agent" {
            return Ok(Self::Agent);
        }
        match value.strip_prefix("unit:") {
            Some(unit) if !unit.is_empty() => Ok(Self::Unit(unit.to_owned())),
            _ => Err(format!(
                "unknown wait condition `{value}`: use `agent` or `unit:<name>`"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// Shell command run in the guest with `/bin/sh -c`.
    pub run: String,
    #[serde(default)]
    pub exit: i32,
    pub stdout_contains: Option<String>,
}

/// How to rerun a failed VM scenario against the exact same image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reproduction {
    pub image: PathBuf,
    pub command: String,
}

/// Where a VM run happens and what it leaves behind.
pub struct VmRun<'a> {
    pub spec: &'a VmSpec,
    /// The image output directory, already built.
    pub image: &'a Path,
    /// Directory for this run's state and logs. Emptied before the run.
    pub dir: &'a Path,
}

/// Builds a test image and returns its output directory.
pub fn build_image(reference: &str, workdir: &Path) -> Result<PathBuf, String> {
    let installable = installable(reference);
    let output = Command::new("nix")
        .args(["build", &installable, "--no-link", "--print-out-paths"])
        .current_dir(workdir)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run nix: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<_> = stderr.lines().collect();
        let tail = &lines[lines.len().saturating_sub(5)..];
        return Err(format!(
            "building image `{reference}` failed: {}",
            tail.join(" | ")
        ));
    }
    let out = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    if !out.join("disk.qcow2").is_file() {
        return Err(format!(
            "image `{reference}` has no disk.qcow2 in {}",
            out.display()
        ));
    }
    Ok(out)
}

/// Turns a repository-relative flake reference into something `nix build` reads as a path.
fn installable(reference: &str) -> String {
    let flake = reference.split('#').next().unwrap_or_default();
    if flake.contains(':') || flake.starts_with('/') || flake.starts_with('.') {
        reference.to_owned()
    } else {
        format!("./{reference}")
    }
}

impl VmRun<'_> {
    /// Boots the VM, waits for the declared conditions and checks the expectations. On failure,
    /// the run directory keeps the console log, the guest journal when the agent answered, and a
    /// screenshot.
    pub fn execute(&self) -> Result<(), String> {
        let firmware = FirmwarePaths::from_env()?;
        check_kvm()?;
        self.prepare_dir(&firmware)?;

        let _tpm = self.spec.tpm.then(|| self.start_swtpm()).transpose()?;
        let args = self.qemu_args(&firmware, pick_cid());
        self.write_interactive_script(&args)?;

        let log = fs::File::create(self.dir.join("qemu.log")).map_err(io_err("qemu.log"))?;
        let qemu = Command::new("qemu-system-x86_64")
            .args(&args.argv)
            .current_dir(self.dir)
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(io_err("qemu.log"))?)
            .stderr(log)
            .spawn()
            .map_err(|e| format!("could not start qemu-system-x86_64: {e}"))?;
        let mut qemu = KillOnDrop(qemu);

        let deadline = Instant::now() + Duration::from_secs(self.spec.timeout_secs);
        let result = self
            .connect(args.cid, &mut qemu.0, deadline)
            .and_then(|mut agent| {
                let result = self.check(&mut agent, deadline);
                if result.is_err() {
                    self.save_journal(&mut agent);
                }
                result
            });
        if result.is_err() {
            // Best effort: a missing screenshot shouldn't hide the real failure.
            let _ = screendump(&self.dir.join("qmp.sock"));
        }
        result
    }

    fn prepare_dir(&self, firmware: &FirmwarePaths) -> Result<(), String> {
        if self.dir.exists() {
            fs::remove_dir_all(self.dir).map_err(io_err("the old run directory"))?;
        }
        fs::create_dir_all(self.dir).map_err(io_err("the run directory"))?;

        let backing = self.image.join("disk.qcow2");
        let status = Command::new("qemu-img")
            .args(["create", "-q", "-f", "qcow2", "-F", "qcow2", "-b"])
            .arg(&backing)
            .arg(self.dir.join("disk.qcow2"))
            .status()
            .map_err(|e| format!("could not run qemu-img: {e}"))?;
        if !status.success() {
            return Err(format!(
                "qemu-img could not create an overlay for {}",
                backing.display()
            ));
        }

        let vars = self.dir.join("ovmf-vars.fd");
        fs::copy(&firmware.vars, &vars).map_err(io_err("the UEFI variables"))?;
        fs::set_permissions(&vars, fs::Permissions::from_mode(0o644))
            .map_err(io_err("the UEFI variables"))
    }

    fn start_swtpm(&self) -> Result<KillOnDrop, String> {
        fs::create_dir_all(self.dir.join("tpm")).map_err(io_err("the TPM state directory"))?;
        let child = Command::new("swtpm")
            .args(swtpm_args())
            .current_dir(self.dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start swtpm: {e}"))?;
        let swtpm = KillOnDrop(child);

        let socket = self.dir.join("swtpm.sock");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            if Instant::now() >= deadline {
                return Err("swtpm did not create its socket within 5s".into());
            }
            thread::sleep(Duration::from_millis(20));
        }
        Ok(swtpm)
    }

    fn qemu_args(&self, firmware: &FirmwarePaths, cid: u32) -> QemuArgs {
        let mut argv: Vec<String> = [
            "-machine",
            "q35,accel=kvm",
            "-cpu",
            "host",
            "-m",
            "2048",
            "-smp",
            "2",
            "-display",
            "none",
            "-monitor",
            "none",
            "-qmp",
            "unix:qmp.sock,server=on,wait=off",
            "-serial",
            "file:console.log",
            "-drive",
            "if=virtio,format=qcow2,file=disk.qcow2",
            "-drive",
            "if=pflash,format=raw,unit=1,file=ovmf-vars.fd",
        ]
        .map(str::to_owned)
        .into();
        argv.extend([
            "-drive".into(),
            format!(
                "if=pflash,format=raw,unit=0,readonly=on,file={}",
                firmware.code.display()
            ),
            "-device".into(),
            format!("vhost-vsock-pci,guest-cid={cid}"),
        ]);
        if self.spec.tpm {
            argv.extend(
                [
                    "-chardev",
                    "socket,id=chrtpm,path=swtpm.sock",
                    "-tpmdev",
                    "emulator,id=tpm0,chardev=chrtpm",
                    "-device",
                    "tpm-crb,tpmdev=tpm0",
                ]
                .map(str::to_owned),
            );
        }
        QemuArgs { argv, cid }
    }

    /// Writes `interactive.sh`, which boots this run's VM with the serial console on the terminal.
    fn write_interactive_script(&self, args: &QemuArgs) -> Result<(), String> {
        let qemu_args: Vec<_> = args
            .argv
            .iter()
            .map(|arg| {
                if arg == "file:console.log" {
                    "mon:stdio".to_owned()
                } else {
                    shell_quote(arg)
                }
            })
            .collect();
        let mut script = String::from(
            "#!/bin/sh\n\
             # Boots the VM from this run with the serial console on this terminal.\n\
             # Run it inside the devenv shell. Quit with Ctrl-a x.\n\
             set -e\n\
             cd \"$(dirname \"$0\")\"\n",
        );
        if self.spec.tpm {
            let swtpm: Vec<_> = swtpm_args().iter().map(|a| shell_quote(a)).collect();
            script.push_str(&format!("swtpm {} &\nsleep 1\n", swtpm.join(" ")));
        }
        script.push_str(&format!(
            "exec qemu-system-x86_64 {}\n",
            qemu_args.join(" ")
        ));

        let path = self.dir.join("interactive.sh");
        fs::write(&path, script).map_err(io_err("interactive.sh"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .map_err(io_err("interactive.sh"))
    }

    fn connect(
        &self,
        cid: u32,
        qemu: &mut Child,
        deadline: Instant,
    ) -> Result<Client<VsockStream>, String> {
        loop {
            if let Ok(Some(status)) = qemu.try_wait() {
                let log = fs::read_to_string(self.dir.join("qemu.log")).unwrap_or_default();
                let last = log.lines().rev().find(|l| !l.trim().is_empty());
                return Err(match last {
                    Some(line) => format!("QEMU exited early ({status}): {}", line.trim()),
                    None => format!("QEMU exited early ({status})"),
                });
            }
            if let Ok(stream) = VsockStream::connect_with_cid_port(cid, distro_agent::DEFAULT_PORT)
            {
                let mut client = Client::new(stream);
                if request(&mut client, &Request::Ping, deadline).is_ok() {
                    return Ok(client);
                }
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "the guest agent did not answer within {}s",
                    self.spec.timeout_secs
                ));
            }
            thread::sleep(Duration::from_millis(500));
        }
    }

    fn check(&self, agent: &mut Client<VsockStream>, deadline: Instant) -> Result<(), String> {
        for condition in &self.spec.wait_for {
            match condition {
                WaitFor::Agent => {}
                WaitFor::Unit(unit) => wait_for_unit(agent, unit, deadline)?,
            }
        }
        for expect in &self.spec.expect {
            check_expect(agent, expect, deadline)?;
        }
        Ok(())
    }

    fn save_journal(&self, agent: &mut Client<VsockStream>) {
        let journal = Request::Run {
            argv: ["journalctl", "-b", "--no-pager"].map(str::to_owned).into(),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        if let Ok(Response::Exited { stdout, .. }) = request(agent, &journal, deadline) {
            // Best effort: the failure itself is already known.
            let _ = fs::write(self.dir.join("journal.log"), stdout);
        }
    }
}

struct QemuArgs {
    argv: Vec<String>,
    cid: u32,
}

struct FirmwarePaths {
    code: PathBuf,
    vars: PathBuf,
}

impl FirmwarePaths {
    fn from_env() -> Result<Self, String> {
        let var = |name: &str| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .ok_or_else(|| format!("{name} is not set; run inside the devenv shell"))
        };
        Ok(Self {
            code: var("OVMF_CODE")?,
            vars: var("OVMF_VARS")?,
        })
    }
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        // The process may already have exited, which is fine.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn swtpm_args() -> [&'static str; 8] {
    [
        "socket",
        "--tpm2",
        "--tpmstate",
        "dir=tpm",
        "--ctrl",
        "type=unixio,path=swtpm.sock",
        "--log",
        "file=swtpm.log",
    ]
}

fn check_kvm() -> Result<(), String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .map(drop)
        .map_err(|e| format!("/dev/kvm is not accessible: {e}"))
}

/// Picks a vsock context ID for a new VM. IDs below 3 are reserved, and IDs must be unique on
/// the host, so mix the process ID with the clock to make collisions between runs unlikely.
fn pick_cid() -> u32 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    1000 + (std::process::id().wrapping_mul(7919) ^ nanos) % 1_000_000_000
}

fn request(
    agent: &mut Client<VsockStream>,
    request: &Request,
    deadline: Instant,
) -> io::Result<Response> {
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_secs(1));
    agent.stream().set_read_timeout(Some(remaining))?;
    agent.request(request)
}

fn run_shell(
    agent: &mut Client<VsockStream>,
    argv: Vec<String>,
    deadline: Instant,
) -> Result<(Option<i32>, String, String), String> {
    let shown = argv.join(" ");
    match request(agent, &Request::Run { argv }, deadline) {
        Ok(Response::Exited {
            code,
            stdout,
            stderr,
        }) => Ok((code, stdout, stderr)),
        Ok(Response::Error { message }) => Err(format!("`{shown}`: {message}")),
        Ok(Response::Pong) => Err(format!("`{shown}`: unexpected response from the agent")),
        Err(e) => Err(format!("`{shown}`: lost the guest agent: {e}")),
    }
}

fn wait_for_unit(
    agent: &mut Client<VsockStream>,
    unit: &str,
    deadline: Instant,
) -> Result<(), String> {
    loop {
        let argv = ["systemctl", "is-active", unit].map(str::to_owned).into();
        let (_, stdout, _) = run_shell(agent, argv, deadline)?;
        let state = stdout.trim();
        if state == "active" {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("unit `{unit}` is not active (state: {state})"));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn check_expect(
    agent: &mut Client<VsockStream>,
    expect: &Expect,
    deadline: Instant,
) -> Result<(), String> {
    let argv = vec!["/bin/sh".into(), "-c".into(), expect.run.clone()];
    let (code, stdout, stderr) = run_shell(agent, argv, deadline)?;
    if code != Some(expect.exit) {
        let got = code.map_or_else(
            || "killed by a signal".to_owned(),
            |c| format!("exit code {c}"),
        );
        let context = stderr.lines().map(str::trim).find(|l| !l.is_empty());
        return Err(match context {
            Some(line) => format!("`{}`: {got}, expected {}: {line}", expect.run, expect.exit),
            None => format!("`{}`: {got}, expected {}", expect.run, expect.exit),
        });
    }
    match &expect.stdout_contains {
        Some(needle) if !stdout.contains(needle.as_str()) => Err(format!(
            "`{}`: stdout does not contain {needle:?}",
            expect.run
        )),
        _ => Ok(()),
    }
}

/// Saves a PNG of the VM's display to `screen.png` next to the QMP socket.
fn screendump(qmp_socket: &Path) -> io::Result<()> {
    let stream = UnixStream::connect(qmp_socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let commands = [
        r#"{"execute":"qmp_capabilities"}"#,
        r#"{"execute":"screendump","arguments":{"filename":"screen.png","format":"png"}}"#,
    ];
    for command in commands {
        writeln!(writer, "{command}")?;
        // Skip asynchronous events until the command's own reply arrives.
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            if line.contains("\"return\"") || line.contains("\"error\"") {
                break;
            }
        }
    }
    Ok(())
}

fn shell_quote(arg: &str) -> String {
    if arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_=,.:/".contains(c))
    {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

fn io_err(what: &'static str) -> impl Fn(io::Error) -> String {
    move |e| format!("could not write {what}: {e}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(text: &str) -> Result<VmSpec, toml::de::Error> {
        toml::from_str(text)
    }

    #[test]
    fn parses_spec_with_defaults() {
        let spec = spec("image = \"factory/nix#fixture\"\n").unwrap();

        assert_eq!(spec.firmware, Firmware::Uefi);
        assert!(!spec.tpm);
        assert!(spec.wait_for.is_empty());
        assert!(spec.expect.is_empty());
        assert_eq!(spec.timeout_secs, 300);
    }

    #[test]
    fn parses_wait_conditions_and_expectations() {
        let spec = spec(
            r#"
            image = "factory/nix#fixture"
            tpm = true
            wait_for = ["agent", "unit:multi-user.target"]
            expect = [{ run = "uname", stdout_contains = "Linux" }]
            "#,
        )
        .unwrap();

        assert_eq!(
            spec.wait_for,
            [WaitFor::Agent, WaitFor::Unit("multi-user.target".into())]
        );
        assert_eq!(
            spec.expect,
            [Expect {
                run: "uname".into(),
                exit: 0,
                stdout_contains: Some("Linux".into()),
            }]
        );
    }

    #[test]
    fn rejects_unknown_wait_condition_and_firmware() {
        for bad in [
            "image = \"x\"\nwait_for = [\"port:80\"]\n",
            "image = \"x\"\nwait_for = [\"unit:\"]\n",
        ] {
            let err = spec(bad).unwrap_err();
            assert!(err.message().starts_with("unknown wait condition"), "{err}");
        }

        let err = spec("image = \"x\"\nfirmware = \"bios\"\n").unwrap_err();
        assert!(err.message().contains("unknown variant `bios`"), "{err}");
    }

    #[test]
    fn installable_prefixes_relative_references() {
        assert_eq!(installable("factory/nix#fixture"), "./factory/nix#fixture");
        assert_eq!(installable("./factory/nix#x"), "./factory/nix#x");
        assert_eq!(installable("/abs/flake#x"), "/abs/flake#x");
        assert_eq!(installable("github:o/r#x"), "github:o/r#x");
    }

    #[test]
    fn shell_quote_leaves_plain_args_and_quotes_the_rest() {
        assert_eq!(shell_quote("-drive"), "-drive");
        assert_eq!(
            shell_quote("if=virtio,file=disk.qcow2"),
            "if=virtio,file=disk.qcow2"
        );
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn cids_are_never_reserved() {
        assert!(pick_cid() >= 1000);
    }
}
