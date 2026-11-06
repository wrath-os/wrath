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
#[serde(try_from = "RawVmSpec")]
pub struct VmSpec {
    pub system: System,
    pub firmware: Firmware,
    pub tpm: bool,
    /// Conditions to wait for, in order, before the expectations run.
    pub wait_for: Vec<WaitFor>,
    pub expect: Vec<Expect>,
    /// Run in order after the expectations.
    pub steps: Vec<Step>,
    /// Where the guest's store comes from.
    pub store: Store,
    /// Limit for booting and checking. Building the image and systems is not counted.
    pub timeout_secs: u64,
}

/// What the VM boots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum System {
    /// A flake reference to a test image, relative to the repository root.
    Image(String),
    /// A configuration directory, built into a test image by `distro build`.
    Config(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Builds the configuration on the host, copies the system into the VM and switches to it.
    /// The plan from the configuration before must contain each of `expect_plan`.
    ApplyConfig {
        config: String,
        expect_plan: Vec<String>,
    },
    /// Runs a command in the VM, like an expectation.
    Run(Expect),
    /// The VM has not rebooted since it booted for the scenario.
    ExpectNoReboot,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVmSpec {
    image: Option<String>,
    config: Option<String>,
    #[serde(default)]
    firmware: Firmware,
    #[serde(default)]
    tpm: bool,
    #[serde(default)]
    wait_for: Vec<WaitFor>,
    #[serde(default)]
    expect: Vec<Expect>,
    #[serde(default)]
    steps: Vec<RawStep>,
    #[serde(default)]
    store: Store,
    #[serde(default = "default_timeout_secs")]
    timeout_secs: u64,
}

impl TryFrom<RawVmSpec> for VmSpec {
    type Error = String;

    fn try_from(raw: RawVmSpec) -> Result<Self, Self::Error> {
        let system = match (raw.image, raw.config) {
            (Some(image), None) => System::Image(image),
            (None, Some(config)) => System::Config(config),
            _ => return Err("name exactly one of `image` or `config`".into()),
        };
        Ok(Self {
            system,
            firmware: raw.firmware,
            tpm: raw.tpm,
            wait_for: raw.wait_for,
            expect: raw.expect,
            steps: raw
                .steps
                .into_iter()
                .map(Step::try_from)
                .collect::<Result<_, _>>()?,
            store: raw.store,
            timeout_secs: raw.timeout_secs,
        })
    }
}

/// Every field a step may have, so scenarios using steps from later milestones can wait for them.
pub const STEP_FIELDS: [&str; 6] = [
    "apply_config",
    "expect_plan",
    "run",
    "exit",
    "stdout_contains",
    "expect_no_reboot",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    apply_config: Option<String>,
    #[serde(default)]
    expect_plan: Vec<String>,
    run: Option<String>,
    #[serde(default)]
    exit: i32,
    stdout_contains: Option<String>,
    expect_no_reboot: Option<bool>,
}

impl TryFrom<RawStep> for Step {
    type Error = String;

    fn try_from(raw: RawStep) -> Result<Self, Self::Error> {
        let run_fields = raw.exit != 0 || raw.stdout_contains.is_some();
        match (raw.apply_config, raw.run, raw.expect_no_reboot) {
            (Some(config), None, None) if !run_fields => Ok(Self::ApplyConfig {
                config,
                expect_plan: raw.expect_plan,
            }),
            (None, Some(run), None) if raw.expect_plan.is_empty() => Ok(Self::Run(Expect {
                run,
                exit: raw.exit,
                stdout_contains: raw.stdout_contains,
            })),
            (None, None, Some(true)) if !run_fields && raw.expect_plan.is_empty() => {
                Ok(Self::ExpectNoReboot)
            }
            _ => Err(
                "a step is one of `apply_config` (with `expect_plan`), `run` (with `exit` \
                 and `stdout_contains`) or `expect_no_reboot = true`"
                    .into(),
            ),
        }
    }
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

/// Where the guest's store comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Store {
    /// A disk image holding the system's whole closure, booted with UEFI.
    #[default]
    Image,
    /// The host's store, mounted read-only with the kernel booted directly. For systems too
    /// large to copy into an image.
    Host,
}

impl Store {
    /// The build targets for booting and for switching to a system.
    fn targets(self) -> (&'static str, &'static str) {
        match self {
            Self::Image => ("test-image", "test-system"),
            Self::Host => ("test-vm", "test-vm-system"),
        }
    }
}

/// How an image output directory boots, from what it contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boot {
    /// `disk.qcow2`, a UEFI-bootable disk.
    Disk,
    /// `kernel`, `initrd` and `kernel-params`, for a system in the host's store.
    Kernel,
}

impl Boot {
    pub fn of(image: &Path) -> Result<Self, String> {
        if image.join("disk.qcow2").is_file() {
            Ok(Self::Disk)
        } else if image.join("kernel").is_file() {
            Ok(Self::Kernel)
        } else {
            Err(format!(
                "{} has neither disk.qcow2 nor kernel",
                image.display()
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub enum WaitFor {
    /// The guest agent answers.
    Agent,
    /// A systemd unit is active.
    Unit(String),
    /// A unit of a user's systemd instance is active.
    UserUnit { user: String, unit: String },
}

impl TryFrom<String> for WaitFor {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value == "agent" {
            return Ok(Self::Agent);
        }
        if let Some(unit) = value.strip_prefix("unit:")
            && !unit.is_empty()
        {
            return Ok(Self::Unit(unit.to_owned()));
        }
        if let Some((user, unit)) = value
            .strip_prefix("user-unit:")
            .and_then(|rest| rest.split_once(':'))
            && !user.is_empty()
            && !unit.is_empty()
        {
            return Ok(Self::UserUnit {
                user: user.to_owned(),
                unit: unit.to_owned(),
            });
        }
        Err(format!(
            "unknown wait condition `{value}`: use `agent`, `unit:<name>` or \
             `user-unit:<user>:<name>`"
        ))
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
    /// For steps that build on the host.
    pub host: Host<'a>,
}

/// Where host-side steps run.
#[derive(Debug, Clone, Copy)]
pub struct Host<'a> {
    /// The `distro` binary that builds and plans configurations.
    pub distro: &'a Path,
    /// The repository root, which configuration paths are relative to.
    pub workdir: &'a Path,
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
    Boot::of(&out)?;
    Ok(out)
}

/// Builds the test image for a configuration with `distro build`.
pub fn build_config_image(host: Host<'_>, config: &str, store: Store) -> Result<PathBuf, String> {
    let (image, _) = store.targets();
    let out = PathBuf::from(
        host.distro(&["build", "--config", config, "--target", image])?
            .trim(),
    );
    Boot::of(&out)?;
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
        let boot = Boot::of(self.image)?;
        self.prepare_dir(boot, &firmware)?;

        let _tpm = self.spec.tpm.then(|| self.start_swtpm()).transpose()?;
        let args = self.qemu_args(boot, &firmware, pick_cid())?;
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

    fn prepare_dir(&self, boot: Boot, firmware: &FirmwarePaths) -> Result<(), String> {
        if self.dir.exists() {
            fs::remove_dir_all(self.dir).map_err(io_err("the old run directory"))?;
        }
        fs::create_dir_all(self.dir).map_err(io_err("the run directory"))?;
        fs::create_dir(self.dir.join(SHARE)).map_err(io_err("the share directory"))?;
        if boot == Boot::Kernel {
            return Ok(());
        }

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

    fn qemu_args(
        &self,
        boot: Boot,
        firmware: &FirmwarePaths,
        cid: u32,
    ) -> Result<QemuArgs, String> {
        let mut argv: Vec<String> = [
            "-machine",
            "q35,accel=kvm",
            "-cpu",
            "host",
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
            "-virtfs",
            "local,path=share,mount_tag=distro-share,security_model=none,readonly=on",
        ]
        .map(str::to_owned)
        .into();
        match boot {
            Boot::Disk => argv.extend([
                "-m".into(),
                "2048".into(),
                "-drive".into(),
                "if=virtio,format=qcow2,file=disk.qcow2".into(),
                "-drive".into(),
                "if=pflash,format=raw,unit=1,file=ovmf-vars.fd".into(),
                "-drive".into(),
                format!(
                    "if=pflash,format=raw,unit=0,readonly=on,file={}",
                    firmware.code.display()
                ),
            ]),
            Boot::Kernel => {
                let params = fs::read_to_string(self.image.join("kernel-params"))
                    .map_err(io_err("kernel-params"))?;
                argv.extend([
                    // The root filesystem and the store's writable layer are in memory.
                    "-m".into(),
                    "4096".into(),
                    "-kernel".into(),
                    self.image.join("kernel").display().to_string(),
                    "-initrd".into(),
                    self.image.join("initrd").display().to_string(),
                    "-append".into(),
                    params.trim().to_owned(),
                    "-virtfs".into(),
                    "local,path=/nix/store,mount_tag=nix-store,security_model=none,readonly=on"
                        .into(),
                ]);
            }
        }
        argv.extend(["-device".into(), format!("vhost-vsock-pci,guest-cid={cid}")]);
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
        Ok(QemuArgs { argv, cid })
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
                WaitFor::Unit(unit) => {
                    let argv = argv(&["systemctl", "is-active", unit]);
                    wait_for_active(agent, argv, &format!("unit `{unit}`"), deadline)?;
                }
                WaitFor::UserUnit { user, unit } => {
                    let machine = format!("{user}@");
                    let argv = argv(&[
                        "systemctl",
                        "--user",
                        "--machine",
                        &machine,
                        "is-active",
                        unit,
                    ]);
                    let what = format!("unit `{unit}` of user `{user}`");
                    wait_for_active(agent, argv, &what, deadline)?;
                }
            }
        }
        for expect in &self.spec.expect {
            check_expect(agent, expect, deadline)?;
        }
        if !self.spec.steps.is_empty() {
            self.run_steps(agent, deadline)?;
        }
        Ok(())
    }

    fn save_journal(&self, agent: &mut Client<VsockStream>) {
        let journal = Request::Run {
            argv: argv(&["journalctl", "-b", "--no-pager"]),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        if let Ok(Response::Exited { stdout, .. }) = request(agent, &journal, deadline) {
            // Best effort: the failure itself is already known.
            let _ = fs::write(self.dir.join("journal.log"), stdout);
        }
    }
}

/// The directory in a run that the guest mounts read-only at [`GUEST_SHARE`].
const SHARE: &str = "share";

/// Where the guest sees the share.
const GUEST_SHARE: &str = "/run/distro-share";

/// The file in the share holding store paths for the guest to import.
const EXPORT: &str = "paths.nar";

impl VmRun<'_> {
    fn run_steps(&self, agent: &mut Client<VsockStream>, deadline: Instant) -> Result<(), String> {
        let booted = boot_id(agent, deadline)?;
        let System::Config(initial) = &self.spec.system else {
            return Err("steps need a scenario with `config`".into());
        };
        let mut current = initial.as_str();
        for step in &self.spec.steps {
            match step {
                Step::ApplyConfig {
                    config,
                    expect_plan,
                } => {
                    self.apply_config(agent, current, config, expect_plan, deadline)?;
                    current = config;
                }
                Step::Run(expect) => check_expect(agent, expect, deadline)?,
                Step::ExpectNoReboot => {
                    if boot_id(agent, deadline)? != booted {
                        return Err("the VM rebooted".into());
                    }
                }
            }
        }
        Ok(())
    }

    /// Checks the plan from `current` to `config`, then builds `config` on the host, copies it
    /// into the VM and switches to it.
    fn apply_config(
        &self,
        agent: &mut Client<VsockStream>,
        current: &str,
        config: &str,
        expect_plan: &[String],
        deadline: Instant,
    ) -> Result<(), String> {
        let plan = self
            .host
            .distro(&["plan", "--config", config, "--against", current])?;
        if let Some(missing) = expect_plan
            .iter()
            .find(|needle| !plan.contains(needle.as_str()))
        {
            return Err(format!(
                "the plan for `{config}` does not contain {missing:?}:\n{plan}"
            ));
        }
        let (_, target) = self.spec.store.targets();
        let system = self
            .host
            .distro(&["build", "--config", config, "--target", target])?;
        let system = system.trim();
        self.copy_in(agent, system, deadline)?;
        let (code, _, stderr) = run_shell(agent, argv(&["distro", "activate", system]), deadline)?;
        if code != Some(0) {
            return Err(format!("switching to `{config}` failed: {}", stderr.trim()));
        }
        Ok(())
    }

    /// Copies the store paths of `system` that the guest lacks into the guest's store, as an
    /// export in the share. The guest already has everything else they refer to.
    fn copy_in(
        &self,
        agent: &mut Client<VsockStream>,
        system: &str,
        deadline: Instant,
    ) -> Result<(), String> {
        // Dependencies come before the paths that refer to them, as importing needs.
        let closure =
            host_command(Command::new("nix-store").args(["--query", "--requisites", system]))?;
        let mut check = argv(&["nix-store", "--check-validity", "--print-invalid"]);
        check.extend(closure.lines().map(str::to_owned));
        let (_, missing, _) = run_shell(agent, check, deadline)?;
        if missing.trim().is_empty() {
            return Ok(());
        }
        // A guest on the host's store already sees the paths and only needs them registered.
        let (export, import) = match self.spec.store {
            Store::Image => ("--export", "--import"),
            Store::Host => ("--dump-db", "--load-db"),
        };
        let file =
            fs::File::create(self.dir.join(SHARE).join(EXPORT)).map_err(io_err("the export"))?;
        host_command(
            Command::new("nix-store")
                .arg(export)
                .args(missing.lines())
                .stdout(file),
        )?;
        let import = format!("nix-store {import} < {GUEST_SHARE}/{EXPORT}");
        let (code, _, stderr) = run_shell(agent, argv(&["/bin/sh", "-c", &import]), deadline)?;
        if code != Some(0) {
            return Err(format!(
                "copying {system} into the VM failed: {}",
                stderr.trim()
            ));
        }
        Ok(())
    }
}

impl Host<'_> {
    /// Runs `distro` on the host and returns its output.
    fn distro(&self, args: &[&str]) -> Result<String, String> {
        host_command(
            Command::new(self.distro)
                .args(args)
                .current_dir(self.workdir),
        )
    }
}

/// Runs a host command to completion and returns its stdout, or its stderr on failure.
fn host_command(command: &mut Command) -> Result<String, String> {
    let shown = format!(
        "{} {}",
        command.get_program().to_string_lossy(),
        command
            .get_args()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run `{shown}`: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "`{shown}` failed ({}):\n{}{}",
        output.status,
        stdout,
        stderr.trim_end()
    ))
}

/// Identifies the current boot of the guest.
fn boot_id(agent: &mut Client<VsockStream>, deadline: Instant) -> Result<String, String> {
    let (_, stdout, _) = run_shell(
        agent,
        argv(&["cat", "/proc/sys/kernel/random/boot_id"]),
        deadline,
    )?;
    Ok(stdout.trim().to_owned())
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
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

/// Polls `argv`, a `systemctl is-active` command, until it prints `active`.
fn wait_for_active(
    agent: &mut Client<VsockStream>,
    argv: Vec<String>,
    what: &str,
    deadline: Instant,
) -> Result<(), String> {
    loop {
        let (_, stdout, _) = run_shell(agent, argv.clone(), deadline)?;
        let state = stdout.trim();
        if state == "active" {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("{what} is not active (state: {state})"));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn check_expect(
    agent: &mut Client<VsockStream>,
    expect: &Expect,
    deadline: Instant,
) -> Result<(), String> {
    let (code, stdout, stderr) = run_shell(agent, argv(&["/bin/sh", "-c", &expect.run]), deadline)?;
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
            "`{}`: stdout does not contain {needle:?}, it was {:?}",
            expect.run,
            truncate(&stdout, 500)
        )),
        _ => Ok(()),
    }
}

/// The first `max` bytes of `text`, cut at a character boundary, with `…` if anything is left.
fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().map(|(at, _)| at).find(|&at| at >= max) {
        Some(at) => format!("{}…", &text[..at]),
        None => text.to_owned(),
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
        assert_eq!(spec.store, Store::Image);
    }

    #[test]
    fn a_config_scenario_can_boot_from_the_host_store() {
        let spec = spec("config = \"local/configs/x\"\nstore = \"host\"\n").unwrap();
        assert_eq!(spec.store, Store::Host);

        let err = spec_err("config = \"x\"\nstore = \"nfs\"\n");
        assert!(err.contains("unknown variant `nfs`"), "{err}");
    }

    #[test]
    fn an_image_directory_says_how_it_boots() {
        let dir = tempfile::tempdir().unwrap();
        let err = Boot::of(dir.path()).unwrap_err();
        assert!(err.contains("neither disk.qcow2 nor kernel"), "{err}");

        fs::write(dir.path().join("kernel"), "").unwrap();
        assert_eq!(Boot::of(dir.path()), Ok(Boot::Kernel));

        fs::write(dir.path().join("disk.qcow2"), "").unwrap();
        assert_eq!(Boot::of(dir.path()), Ok(Boot::Disk));
    }

    fn spec_err(text: &str) -> String {
        spec(text).unwrap_err().message().to_owned()
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
    fn truncate_cuts_long_output_at_a_character() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("exactly", 7), "exactly");
        assert_eq!(truncate("åäö and more", 3), "åä…");
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

    #[test]
    fn a_config_scenario_has_steps() {
        let spec = spec(
            r#"
            config = "fixtures/configs/basic"
            wait_for = ["user-unit:demo:niri.service"]
            steps = [
              { apply_config = "fixtures/configs/basic-changed", expect_plan = ["8081"] },
              { run = "curl localhost", stdout_contains = "200" },
              { expect_no_reboot = true },
            ]
            "#,
        )
        .unwrap();

        assert_eq!(spec.system, System::Config("fixtures/configs/basic".into()));
        assert_eq!(
            spec.wait_for,
            [WaitFor::UserUnit {
                user: "demo".into(),
                unit: "niri.service".into()
            }]
        );
        assert_eq!(
            spec.steps,
            [
                Step::ApplyConfig {
                    config: "fixtures/configs/basic-changed".into(),
                    expect_plan: vec!["8081".into()],
                },
                Step::Run(Expect {
                    run: "curl localhost".into(),
                    exit: 0,
                    stdout_contains: Some("200".into()),
                }),
                Step::ExpectNoReboot,
            ]
        );
    }

    #[test]
    fn a_scenario_names_an_image_or_a_config() {
        for bad in ["", "image = \"x\"\nconfig = \"y\"\n"] {
            let err = spec(bad).unwrap_err();
            assert!(
                err.message().contains("exactly one of `image` or `config`"),
                "{err}"
            );
        }
    }

    #[test]
    fn a_step_does_one_thing() {
        for bad in [
            "config = \"c\"\nsteps = [{ run = \"x\", apply_config = \"y\" }]\n",
            "config = \"c\"\nsteps = [{}]\n",
            "config = \"c\"\nsteps = [{ expect_no_reboot = false }]\n",
            "config = \"c\"\nsteps = [{ run = \"x\", expect_plan = [\"y\"] }]\n",
        ] {
            let err = spec(bad).unwrap_err();
            assert!(err.message().contains("step"), "{bad}: {err}");
        }
    }

    #[test]
    fn user_units_name_the_user() {
        let err = spec("image = \"x\"\nwait_for = [\"user-unit:niri.service\"]\n").unwrap_err();
        assert!(err.message().starts_with("unknown wait condition"), "{err}");
    }
}
