//! Running scenarios in QEMU virtual machines.
//!
//! A scenario names a configuration, which `wrath build` makes into a UEFI-bootable disk image
//! from wrath's own store whose system runs the guest agent. Later systems are copied into the
//! guest from the host's store. Each run gets its own directory with a copy-on-write overlay of
//! the disk, writable UEFI variables, an optional software TPM, logs, and a script that boots the
//! same VM interactively.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use vsock::VsockStream;
use wrath_agent::{Client, Request, Response};
use wrath_build::chain;
use wrath_build::store::{Lab, STORE_DIR, StorePath};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawVmSpec")]
pub struct VmSpec {
    /// The configuration directory the VM boots, relative to the repository root.
    pub config: String,
    pub firmware: Firmware,
    pub tpm: bool,
    /// Conditions to wait for, in order, before the expectations run.
    pub wait_for: Vec<WaitFor>,
    pub expect: Vec<Expect>,
    /// Run in order after the expectations.
    pub steps: Vec<Step>,
    /// Sizes of empty disks attached to the VM, such as `8G`, for `wrath install` to lay out.
    pub blank_disks: Vec<String>,
    /// Whether the guest reaches a cache of what the host built, at `http://10.0.2.100`, and finds
    /// the `[caches]` table that trusts it in `/run/wrath-share/cache.toml`.
    pub cache: bool,
    /// Limit for booting and checking. Building the image and systems is not counted.
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Builds the configuration on the host, copies the system into the VM and switches to it.
    /// The plan from the configuration before must contain each of `expect_plan`.
    /// With `try_for`, it tries the system for that long instead, as `wrath try --for`. With a
    /// nonzero `exit`, the switch must fail with it.
    ApplyConfig {
        config: String,
        expect_plan: Vec<String>,
        try_for: Option<String>,
        exit: i32,
    },
    /// Builds `config` on the host and switches the VM to it with `wrath apply --host`, over SSH
    /// as root with a key made for the run, and leaves the configuration in
    /// `/run/wrath-share/configs/<name>`. The VM's configuration needs SSH and DHCP.
    ApplyHost { config: String },
    /// Applies a user's own configuration in the VM as `user`, without root. Wrath's own store
    /// can't do this yet, so the step fails with a reason.
    ApplyHome {
        config: String,
        user: String,
        try_for: Option<String>,
    },
    /// Builds `config` on the host and copies it into the guest's store, leaving its path in
    /// `/run/wrath-share/system` and the configuration in `/run/wrath-share/configs/<name>`,
    /// for a step that installs it.
    PrepareBuild {
        config: String,
        /// What to build: `test-generation`, the default, `generation`, `image` or `test-image`.
        target: Option<String>,
    },
    /// Stops the VM and boots what was installed on its blank disks, with UEFI, typing
    /// `passphrase` on the console when the disks ask for one.
    /// `missing` leaves out those blank disks, by index, to boot with them gone.
    BootInstalled {
        passphrase: Option<String>,
        missing: Vec<usize>,
        /// A private key, relative to the repository, that logs in to the initrd over SSH, where
        /// the passphrase is typed instead of at the console.
        ssh_key: Option<String>,
        /// The public key the initrd must present, relative to the repository.
        ssh_host_key: Option<String>,
    },
    /// Reboots the VM from inside, as someone would, and waits for it to come back.
    Reboot,
    /// Soft-reboots the VM, starting its userspace again on the same kernel, and waits for it.
    SoftReboot,
    /// Runs a command in the VM, like an expectation.
    Run(Expect),
    /// The VM has not rebooted since it booted for the scenario.
    ExpectNoReboot,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVmSpec {
    config: String,
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
    /// Accepted for scenarios that still say `store = "wrath"`, the only store.
    #[serde(default, rename = "store")]
    _store: Option<Store>,
    #[serde(default)]
    blank_disks: Vec<String>,
    #[serde(default)]
    cache: bool,
    #[serde(default = "default_timeout_secs")]
    timeout_secs: u64,
}

impl TryFrom<RawVmSpec> for VmSpec {
    type Error = String;

    fn try_from(raw: RawVmSpec) -> Result<Self, Self::Error> {
        Ok(Self {
            config: raw.config,
            firmware: raw.firmware,
            tpm: raw.tpm,
            wait_for: raw.wait_for,
            expect: raw.expect,
            steps: raw
                .steps
                .into_iter()
                .map(Step::try_from)
                .collect::<Result<_, _>>()?,
            blank_disks: raw.blank_disks,
            cache: raw.cache,
            timeout_secs: raw.timeout_secs,
        })
    }
}

/// Every field a step may have, so scenarios using steps from later milestones can wait for them.
pub const STEP_FIELDS: [&str; 20] = [
    "run_as",
    "apply_config",
    "apply_host",
    "prepare_build",
    "target",
    "boot_installed",
    "passphrase",
    "missing",
    "ssh_key",
    "ssh_host_key",
    "apply_home",
    "as_user",
    "expect_plan",
    "try_for",
    "run",
    "exit",
    "stdout_contains",
    "expect_no_reboot",
    "reboot",
    "soft_reboot",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    apply_config: Option<String>,
    apply_host: Option<String>,
    prepare_build: Option<String>,
    target: Option<String>,
    boot_installed: Option<bool>,
    passphrase: Option<String>,
    #[serde(default)]
    missing: Vec<usize>,
    ssh_key: Option<String>,
    ssh_host_key: Option<String>,
    apply_home: Option<String>,
    as_user: Option<String>,
    #[serde(default)]
    expect_plan: Vec<String>,
    try_for: Option<String>,
    run: Option<String>,
    run_as: Option<String>,
    #[serde(default)]
    exit: i32,
    stdout_contains: Option<String>,
    expect_no_reboot: Option<bool>,
    reboot: Option<bool>,
    soft_reboot: Option<bool>,
}

impl TryFrom<RawStep> for Step {
    type Error = String;

    fn try_from(raw: RawStep) -> Result<Self, Self::Error> {
        if let Some(config) = raw.prepare_build {
            return Ok(Self::PrepareBuild {
                config,
                target: raw.target,
            });
        }
        if raw.boot_installed == Some(true) {
            return Ok(Self::BootInstalled {
                passphrase: raw.passphrase,
                missing: raw.missing,
                ssh_key: raw.ssh_key,
                ssh_host_key: raw.ssh_host_key,
            });
        }
        if raw.reboot == Some(true) {
            return Ok(Self::Reboot);
        }
        if raw.soft_reboot == Some(true) {
            return Ok(Self::SoftReboot);
        }
        if let Some(config) = raw.apply_host {
            return Ok(Self::ApplyHost { config });
        }
        if let Some(config) = raw.apply_home {
            let user = raw
                .as_user
                .ok_or("`apply_home` needs `as_user`, the user it belongs to")?;
            return Ok(Self::ApplyHome {
                config,
                user,
                try_for: raw.try_for,
            });
        }
        let run_fields = raw.exit != 0 || raw.stdout_contains.is_some();
        let apply_fields = !raw.expect_plan.is_empty() || raw.try_for.is_some();
        match (raw.apply_config, raw.run, raw.expect_no_reboot) {
            (Some(config), None, None) if raw.stdout_contains.is_none() => Ok(Self::ApplyConfig {
                config,
                expect_plan: raw.expect_plan,
                try_for: raw.try_for,
                exit: raw.exit,
            }),
            (None, Some(run), None) if !apply_fields => Ok(Self::Run(Expect {
                run: match raw.run_as {
                    // As the user, through su, which root runs without a password.
                    Some(user) => format!("su {user} -c '{}'", run.replace('\'', "'\\''")),
                    None => run,
                },
                exit: raw.exit,
                stdout_contains: raw.stdout_contains,
            })),
            (None, None, Some(true)) if !run_fields && !apply_fields => Ok(Self::ExpectNoReboot),
            _ => Err(
                "a step is one of `apply_config` (with `expect_plan`, `try_for` and `exit`), \
                 `apply_host`, `run` (with `run_as`, `exit` and `stdout_contains`), \
                 `reboot = true` or `expect_no_reboot = true`"
                    .into(),
            ),
        }
    }
}

fn default_timeout_secs() -> u64 {
    300
}

/// An [`Step::ApplyConfig`] as it runs.
struct Switching<'a> {
    config: &'a str,
    expect_plan: &'a [String],
    try_for: Option<&'a str>,
    exit: i32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Firmware {
    #[default]
    Uefi,
}

/// The only store a scenario may name: wrath's own. The guest boots a disk image from it and
/// sees the host's store read-only at `/run/wrath-store`, from which later systems are copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Store {
    Wrath,
}

/// What `wrath build` is given for `target`: a test generation, which the VM switches to or
/// installs, unless it names another of `generation`, `image` or `test-image`.
fn build_flags(target: Option<&str>) -> Result<&'static [&'static str], String> {
    match target.unwrap_or("test-generation") {
        "test-generation" => Ok(&["--test"]),
        "generation" => Ok(&[]),
        "test-image" => Ok(&["--image", "--test"]),
        "image" => Ok(&["--image"]),
        other => Err(format!("unknown build target `{other}`")),
    }
}

/// How the VM boots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boot {
    /// The raw disk image the configuration was built into, with UEFI.
    Image,
    /// What `wrath install` put on the VM's blank disks, with UEFI.
    Installed,
}

/// Checks that `image` is a disk image file to boot.
fn check_image(image: &Path) -> Result<(), String> {
    if image.is_file() {
        Ok(())
    } else {
        Err(format!("{} is not a disk image", image.display()))
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
    /// The disk image to boot, already built.
    pub image: &'a Path,
    /// Directory for this run's state and logs. Emptied before the run.
    pub dir: &'a Path,
    /// For steps that build on the host.
    pub host: Host<'a>,
}

/// Where host-side steps run.
#[derive(Debug, Clone, Copy)]
pub struct Host<'a> {
    /// The `wrath` binary that builds and plans configurations.
    pub wrath: &'a Path,
    /// The repository root, which configuration paths are relative to.
    pub workdir: &'a Path,
}

/// Builds the disk image that boots a configuration, with `wrath build`.
pub fn build_config_image(host: Host<'_>, config: &str) -> Result<PathBuf, String> {
    let out = PathBuf::from(
        host.wrath(&["build", "--config", config, "--image", "--test"])?
            .trim(),
    );
    check_image(&out)?;
    Ok(out)
}

impl VmRun<'_> {
    /// Boots the VM, waits for the declared conditions and checks the expectations. On failure,
    /// the run directory keeps the console log, the guest journal when the agent answered, and a
    /// screenshot.
    pub fn execute(&self) -> Result<(), String> {
        let firmware = FirmwarePaths::from_env()?;
        check_kvm()?;
        check_image(self.image)?;
        self.prepare_dir(&firmware)?;

        let _tpm = self.spec.tpm.then(|| self.start_swtpm()).transpose()?;
        let args = self.qemu_args(Boot::Image, &firmware, pick_cid(), &[])?;
        self.write_interactive_script(&args)?;

        let mut qemu = self.spawn(&args)?;

        let deadline = Instant::now() + Duration::from_secs(self.spec.timeout_secs);
        let result = self
            .connect(args.cid, &mut qemu.0, deadline)
            .and_then(|mut agent| {
                let result = self.check(&mut qemu, &mut agent, &firmware, args.cid, deadline);
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

    fn spawn(&self, args: &QemuArgs) -> Result<KillOnDrop, String> {
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("qemu.log"))
            .map_err(io_err("qemu.log"))?;
        let qemu = Command::new("qemu-system-x86_64")
            .args(&args.argv)
            .current_dir(self.dir)
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(io_err("qemu.log"))?)
            .stderr(log)
            .spawn()
            .map_err(|e| format!("could not start qemu-system-x86_64: {e}"))?;
        Ok(KillOnDrop(qemu))
    }

    fn prepare_dir(&self, firmware: &FirmwarePaths) -> Result<(), String> {
        if self.dir.exists() {
            fs::remove_dir_all(self.dir).map_err(io_err("the old run directory"))?;
        }
        fs::create_dir_all(self.dir).map_err(io_err("the run directory"))?;
        fs::create_dir(self.dir.join(SHARE)).map_err(io_err("the share directory"))?;
        // The configuration it boots, as later steps share theirs.
        self.share_config(&self.spec.config)?;
        if self.spec.cache {
            let table = &crate::served::cache(self.host)?.table;
            fs::write(self.dir.join(SHARE).join("cache.toml"), table)
                .map_err(io_err("the cache's table"))?;
        }
        for (n, size) in self.spec.blank_disks.iter().enumerate() {
            let status = Command::new("qemu-img")
                .args(["create", "-q", "-f", "qcow2"])
                .arg(self.dir.join(format!("blank{n}.qcow2")))
                .arg(size)
                .status()
                .map_err(|e| format!("could not run qemu-img: {e}"))?;
            if !status.success() {
                return Err(format!("qemu-img could not create a {size} disk"));
            }
        }
        let status = Command::new("qemu-img")
            .args(["create", "-q", "-f", "qcow2", "-F", "raw", "-b"])
            .arg(self.image)
            .arg(self.dir.join("disk.qcow2"))
            .status()
            .map_err(|e| format!("could not run qemu-img: {e}"))?;
        if !status.success() {
            return Err(format!(
                "qemu-img could not create an overlay for {}",
                self.image.display()
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
        missing: &[usize],
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
            "local,path=share,mount_tag=wrath-share,security_model=none,readonly=on",
        ]
        .map(str::to_owned)
        .into();
        match boot {
            Boot::Image => {
                argv.extend([
                    "-m".into(),
                    "2048".into(),
                    "-drive".into(),
                    "if=virtio,format=qcow2,file=disk.qcow2".into(),
                ]);
                argv.extend(uefi_drives(firmware));
                let store = self.host.store()?;
                argv.extend([
                    "-virtfs".into(),
                    format!(
                        "local,path={},mount_tag=wrath-store,security_model=none,readonly=on",
                        store.display()
                    ),
                ]);
                // It reaches nothing but the cache it asked for, and only the host reaches it, at
                // its SSH port.
                let mut nic = String::from("user,model=virtio-net-pci,restrict=on");
                if self.reached_over_ssh() {
                    nic.push_str(&format!(",hostfwd=tcp:127.0.0.1:{}-:22", ssh_port(cid)));
                }
                if self.spec.cache {
                    let port = crate::served::cache(self.host)?.port;
                    let guest = crate::served::GUEST;
                    // A command for each connection: a `tcp:` target is one connection for all.
                    nic.push_str(&format!(
                        ",guestfwd=tcp:{guest}:80-cmd:socat - TCP:127.0.0.1:{port}"
                    ));
                }
                if self.reached_over_ssh() || self.spec.cache {
                    argv.extend(["-nic".into(), nic]);
                }
            }
            Boot::Installed => {
                // The console takes input, so a passphrase can be typed at it.
                let serial = argv.iter().position(|arg| arg == "file:console.log");
                if let Some(at) = serial {
                    argv.splice(
                        at - 1..=at,
                        [
                            "-chardev".into(),
                            "socket,id=serial0,path=serial.sock,server=on,wait=off,\
                             logfile=console.log"
                                .into(),
                            "-serial".into(),
                            "chardev:serial0".into(),
                        ],
                    );
                }
                argv.extend(["-m".into(), "2048".into()]);
                argv.extend(uefi_drives(firmware));
            }
        }
        for n in (0..self.spec.blank_disks.len()).filter(|n| !missing.contains(n)) {
            argv.extend([
                "-drive".into(),
                format!("if=virtio,format=qcow2,file=blank{n}.qcow2"),
            ]);
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
            if let Ok(stream) = VsockStream::connect_with_cid_port(cid, wrath_agent::DEFAULT_PORT) {
                let mut client = Client::new(stream);
                if request(&mut client, &Request::Ping, deadline).is_ok() {
                    return Ok(client);
                }
            }
            if let Some(stuck) = stuck(&self.dir.join("console.log")) {
                return Err(format!("the guest won't come up: {stuck}"));
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

    fn check(
        &self,
        qemu: &mut KillOnDrop,
        agent: &mut Client<VsockStream>,
        firmware: &FirmwarePaths,
        cid: u32,
        deadline: Instant,
    ) -> Result<(), String> {
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
            self.run_steps(qemu, agent, firmware, cid, deadline)?;
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

/// How the installed system's disks are unlocked when it boots.
struct Unlock<'a> {
    passphrase: Option<&'a str>,
    ssh_key: Option<&'a str>,
    ssh_host_key: Option<&'a str>,
}

/// The port the initrd's SSH listens on, as `unlock.ssh.port` defaults to.
const INITRD_SSH: u16 = 2222;

/// The directory in a run that the guest mounts read-only at `/run/wrath-share`.
const SHARE: &str = "share";

impl VmRun<'_> {
    fn run_steps(
        &self,
        qemu: &mut KillOnDrop,
        agent: &mut Client<VsockStream>,
        firmware: &FirmwarePaths,
        cid: u32,
        deadline: Instant,
    ) -> Result<(), String> {
        let mut booted = boot_id(agent, deadline)?;
        let mut current = self.spec.config.as_str();
        for step in &self.spec.steps {
            match step {
                Step::ApplyConfig {
                    config,
                    expect_plan,
                    try_for,
                    exit,
                } => {
                    let switch = Switching {
                        config,
                        expect_plan,
                        try_for: try_for.as_deref(),
                        exit: *exit,
                    };
                    self.apply_config(agent, current, switch, deadline)?;
                    current = config;
                }
                Step::ApplyHost { config } => {
                    self.apply_host(agent, config, ssh_port(cid), deadline)?;
                    current = config;
                }
                Step::ApplyHome { .. } => {
                    return Err("homes in the guest don't work with wrath's own store yet".into());
                }
                Step::PrepareBuild { config, target } => {
                    self.prepare_build(agent, config, target.as_deref(), deadline)?;
                }
                Step::BootInstalled {
                    passphrase,
                    missing,
                    ssh_key,
                    ssh_host_key,
                } => {
                    let unlock = Unlock {
                        passphrase: passphrase.as_deref(),
                        ssh_key: ssh_key.as_deref(),
                        ssh_host_key: ssh_host_key.as_deref(),
                    };
                    self.boot_installed(qemu, agent, firmware, &unlock, missing, deadline)?;
                    booted = boot_id(agent, deadline)?;
                }
                Step::Reboot => {
                    *agent = self.reboot(agent, qemu, cid, &booted, deadline)?;
                    booted = boot_id(agent, deadline)?;
                }
                Step::SoftReboot => *agent = self.soft_reboot(agent, qemu, cid, deadline)?,
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
        Switching {
            config,
            expect_plan,
            try_for,
            exit,
        }: Switching,
        deadline: Instant,
    ) -> Result<(), String> {
        let plan = self
            .host
            .wrath(&["plan", "--config", config, "--against", current])?;
        if let Some(missing) = expect_plan
            .iter()
            .find(|needle| !plan.contains(needle.as_str()))
        {
            return Err(format!(
                "the plan for `{config}` does not contain {missing:?}:\n{plan}"
            ));
        }
        let name = self.copy_generation(agent, config, None, deadline)?;
        let system = format!("{STORE_DIR}/{name}");
        let system = system.as_str();
        let switch = match try_for {
            Some(duration) => argv(&["wrath", "try", "-y", "--for", duration, system]),
            None => argv(&["wrath", "apply", "-y", system]),
        };
        let (code, _, stderr) = run_shell(agent, switch, deadline)?;
        match code {
            Some(code) if code == exit => Ok(()),
            _ if exit == 0 => Err(format!("switching to `{config}` failed: {}", stderr.trim())),
            _ => Err(format!(
                "switching to `{config}` was to exit with {exit}, but exited with {code:?}: {}",
                stderr.trim()
            )),
        }
    }

    /// Reboots the VM with `systemctl reboot` and returns the agent of its next boot, after
    /// `booted`.
    fn reboot(
        &self,
        agent: &mut Client<VsockStream>,
        qemu: &mut KillOnDrop,
        cid: u32,
        booted: &str,
        deadline: Instant,
    ) -> Result<Client<VsockStream>, String> {
        // The agent goes down with it, so it may never answer.
        let _ = run_shell(agent, argv(&["systemctl", "reboot"]), deadline);
        loop {
            let mut next = self.connect(cid, &mut qemu.0, deadline)?;
            // Until it goes down, the agent of the boot before still answers.
            if boot_id(&mut next, deadline).is_ok_and(|id| id != booted) {
                return Ok(next);
            }
            if Instant::now() >= deadline {
                return Err("the VM didn't come back from its reboot".into());
            }
            thread::sleep(Duration::from_secs(1));
        }
    }

    /// Soft-reboots the VM with `systemctl soft-reboot` and returns the agent of the userspace it
    /// starts, which keeps the boot's id: systemd counts soft reboots instead.
    fn soft_reboot(
        &self,
        agent: &mut Client<VsockStream>,
        qemu: &mut KillOnDrop,
        cid: u32,
        deadline: Instant,
    ) -> Result<Client<VsockStream>, String> {
        let before = soft_reboots(agent, deadline)?;
        // The agent goes down with userspace, so it may never answer.
        let _ = run_shell(agent, argv(&["systemctl", "soft-reboot"]), deadline);
        loop {
            let mut next = self.connect(cid, &mut qemu.0, deadline)?;
            if soft_reboots(&mut next, deadline).is_ok_and(|count| count > before) {
                return Ok(next);
            }
            if Instant::now() >= deadline {
                return Err("the VM didn't come back from its soft reboot".into());
            }
            thread::sleep(Duration::from_secs(1));
        }
    }

    /// Stops the VM and boots its blank disks, onto which `wrath install` put a system, typing
    /// `passphrase` on the console when the disks ask for it.
    fn boot_installed(
        &self,
        qemu: &mut KillOnDrop,
        agent: &mut Client<VsockStream>,
        firmware: &FirmwarePaths,
        unlock: &Unlock,
        missing: &[usize],
        deadline: Instant,
    ) -> Result<(), String> {
        let _ = qemu.0.kill();
        let _ = qemu.0.wait();
        // The live system's console stays, beside the installed one's.
        let _ = fs::rename(
            self.dir.join("console.log"),
            self.dir.join("console-live.log"),
        );
        let mut args = self.qemu_args(Boot::Installed, firmware, pick_cid(), missing)?;
        let port = 20000 + (args.cid % 20000) as u16;
        if unlock.ssh_key.is_some() {
            args.argv.extend([
                "-nic".into(),
                format!("user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:{port}-:{INITRD_SSH}"),
            ]);
        }
        *qemu = self.spawn(&args)?;
        match (unlock.passphrase, unlock.ssh_key) {
            (Some(passphrase), Some(key)) => {
                self.unlock_over_ssh(port, key, unlock.ssh_host_key, passphrase, deadline)?
            }
            (Some(passphrase), None) => self.type_at_prompt(passphrase, deadline)?,
            (None, _) => {}
        }
        *agent = self.connect(args.cid, &mut qemu.0, deadline)?;
        Ok(())
    }

    /// Logs in to the initrd over SSH on `port` with `key` until it answers, and types
    /// `passphrase` at the disks' prompt there.
    fn unlock_over_ssh(
        &self,
        port: u16,
        key: &str,
        host_key: Option<&str>,
        passphrase: &str,
        deadline: Instant,
    ) -> Result<(), String> {
        // Only the initrd presenting `host_key` is trusted, when there is one.
        let known = self.dir.join("known_hosts");
        let checking = match host_key {
            Some(host_key) => {
                let key = fs::read_to_string(self.host.workdir.join(host_key))
                    .map_err(io_err("the initrd's host key"))?;
                let mut parts = key.split_whitespace();
                let (kind, key) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                fs::write(&known, format!("[127.0.0.1]:{port} {kind} {key}\n"))
                    .map_err(io_err("known_hosts"))?;
                "StrictHostKeyChecking=yes"
            }
            None => "StrictHostKeyChecking=no",
        };
        // ssh refuses a private key others can read.
        let copy = self.dir.join("ssh-key");
        fs::copy(self.host.workdir.join(key), &copy).map_err(io_err("the SSH key"))?;
        fs::set_permissions(&copy, fs::Permissions::from_mode(0o600))
            .map_err(io_err("the SSH key"))?;
        loop {
            let mut ssh = Command::new("ssh")
                .args(["-tt", "-o", checking, "-o"])
                .arg(format!("UserKnownHostsFile={}", known.display()))
                .args(["-o", "ConnectTimeout=3", "-i"])
                .arg(&copy)
                .args(["-p", &port.to_string(), "root@127.0.0.1"])
                .arg("systemd-tty-ask-password-agent --query")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("could not run ssh: {e}"))?;
            thread::sleep(Duration::from_secs(3));
            if let Some(mut stdin) = ssh.stdin.take() {
                // The agent over SSH ends the answer at a newline; a serial console sends `\r`.
                let _ = stdin.write_all(format!("{passphrase}\n").as_bytes());
            }
            if ssh.wait().is_ok_and(|status| status.success()) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("the installed system's initrd never let SSH unlock it".into());
            }
            thread::sleep(Duration::from_secs(2));
        }
    }

    /// Waits for the console to ask for a passphrase, then types `passphrase` at it.
    fn type_at_prompt(&self, passphrase: &str, deadline: Instant) -> Result<(), String> {
        let console = self.dir.join("console.log");
        loop {
            let text = fs::read_to_string(&console).unwrap_or_default();
            if text.contains("passphrase") {
                break;
            }
            if Instant::now() >= deadline {
                return Err("the installed system never asked for its passphrase".into());
            }
            thread::sleep(Duration::from_millis(500));
        }
        let mut serial =
            UnixStream::connect(self.dir.join("serial.sock")).map_err(io_err("the console"))?;
        // A key at a time, as someone types, then Enter, which a serial console sends as `\r`.
        thread::sleep(Duration::from_secs(1));
        for key in passphrase.bytes().chain(*b"\r") {
            serial.write_all(&[key]).map_err(io_err("the console"))?;
            thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    /// Builds `config` on the host as `target`, a test generation unless said, copies it into the
    /// guest's store and leaves its path in `/run/wrath-share/system` and the configuration in
    /// `/run/wrath-share/configs/<name>`, for a step that installs it.
    fn prepare_build(
        &self,
        agent: &mut Client<VsockStream>,
        config: &str,
        target: Option<&str>,
        deadline: Instant,
    ) -> Result<(), String> {
        let name = self.copy_generation(agent, config, target, deadline)?;
        fs::write(
            self.dir.join(SHARE).join("system"),
            format!("{STORE_DIR}/{name}\n"),
        )
        .map_err(io_err("the system's path"))?;
        self.share_config(config)
    }

    /// Copies `config` into `/run/wrath-share/configs/<name>`.
    fn share_config(&self, config: &str) -> Result<(), String> {
        let name = Path::new(config)
            .file_name()
            .ok_or("a configuration needs a name")?;
        copy_dir(
            &self.host.workdir.join(config),
            &self.dir.join(SHARE).join("configs").join(name),
        )
    }

    fn reached_over_ssh(&self) -> bool {
        self.spec
            .steps
            .iter()
            .any(|step| matches!(step, Step::ApplyHost { .. }))
    }

    /// Switches the VM to `config` from the host with `wrath apply --host`, over SSH on `port`,
    /// giving root a key made for the run first.
    fn apply_host(
        &self,
        agent: &mut Client<VsockStream>,
        config: &str,
        port: u16,
        deadline: Instant,
    ) -> Result<(), String> {
        let key = self.dir.join("ssh-host-key");
        if !key.exists() {
            host_command(
                Command::new("ssh-keygen")
                    .args(["-q", "-t", "ed25519", "-N", "", "-C", "wrath-check", "-f"])
                    .arg(&key),
            )?;
            let public =
                fs::read_to_string(key.with_extension("pub")).map_err(io_err("the SSH key"))?;
            let authorize = format!(
                "install -d -m 700 /root/.ssh && echo {} >> /root/.ssh/authorized_keys",
                shell_quote(public.trim())
            );
            let (code, _, stderr) =
                run_shell(agent, argv(&["/bin/sh", "-c", &authorize]), deadline)?;
            if code != Some(0) {
                return Err(format!("giving root the SSH key failed: {}", stderr.trim()));
            }
        }
        let ssh = format!(
            "ssh -p {port} -i {} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
             -o LogLevel=ERROR -o BatchMode=yes -o ConnectTimeout=10",
            key.display()
        );
        host_command(
            Command::new(self.host.wrath)
                .args([
                    "apply",
                    "--config",
                    config,
                    "--host",
                    "root@127.0.0.1",
                    "-y",
                ])
                .env("WRATH_SSH", ssh)
                .current_dir(self.host.workdir),
        )
        .map_err(|e| format!("applying `{config}` over SSH failed: {e}"))?;
        self.share_config(config)
    }

    /// Builds `config` on the host as `target`, copies the generation into the guest's store and
    /// returns its name there.
    fn copy_generation(
        &self,
        agent: &mut Client<VsockStream>,
        config: &str,
        target: Option<&str>,
        deadline: Instant,
    ) -> Result<String, String> {
        let mut args = vec!["build", "--config", config];
        args.extend(build_flags(target)?);
        let built = self.host.wrath(&args)?;
        let name = Path::new(built.trim())
            .file_name()
            .ok_or("a generation has a name")?
            .to_string_lossy()
            .into_owned();
        self.copy_from_store(agent, &name, deadline)?;
        Ok(name)
    }

    /// Copies generation `name` and what it needs from the host's store into the guest's, where
    /// it lacks them.
    fn copy_from_store(
        &self,
        agent: &mut Client<VsockStream>,
        name: &str,
        deadline: Instant,
    ) -> Result<(), String> {
        let lab = Lab::new(self.host.work());
        let generation = StorePath::from_hash(name);
        let closure = chain::runtime_closure(&lab, &generation)?;
        let hashes: Vec<&str> = closure.iter().map(|path| path.hash()).collect();
        let (_, have, _) = run_shell(agent, argv(&["ls", STORE_DIR]), deadline)?;
        let Some(copy) = copy_command(&have, &hashes) else {
            return Ok(());
        };
        let (code, _, stderr) = run_shell(agent, argv(&["/bin/sh", "-c", &copy]), deadline)?;
        if code != Some(0) {
            return Err(format!("copying into the VM failed: {}", stderr.trim()));
        }
        Ok(())
    }
}

/// The command that copies the paths of `closure` the guest lacks, as `ls /wrath/store` there says,
/// from the host's store into its own, or `None` when it has them all.
fn copy_command(have: &str, closure: &[&str]) -> Option<String> {
    let have: BTreeSet<&str> = have.lines().collect();
    let missing: Vec<String> = closure
        .iter()
        .filter(|hash| !have.contains(*hash))
        .map(|hash| format!("{GUEST_STORE}/{hash}"))
        .collect();
    (!missing.is_empty()).then(|| format!("cp -a {} {STORE_DIR}/", missing.join(" ")))
}

/// The host's port that reaches the VM's SSH, apart for VMs that run at once.
fn ssh_port(cid: u32) -> u16 {
    30000 + (cid % 20000) as u16
}

/// Where the guest sees the host's store, from wrath's own builder.
const GUEST_STORE: &str = "/run/wrath-store";

impl Host<'_> {
    /// Where wrath's own builder keeps its store on the host.
    fn work(&self) -> PathBuf {
        self.workdir.join("build/work")
    }

    fn store(&self) -> Result<PathBuf, String> {
        let store = self.work().join("store");
        store
            .canonicalize()
            .map_err(|e| format!("{}: {e}", store.display()))
    }

    /// Runs `wrath` on the host and returns its output.
    pub(crate) fn wrath(&self, args: &[&str]) -> Result<String, String> {
        host_command(
            Command::new(self.wrath)
                .args(args)
                .current_dir(self.workdir),
        )
    }
}

/// Copies the directory `from` to `to`, files and subdirectories.
fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for entry in fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", from.display()))?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        if source.is_dir() {
            copy_dir(&source, &target)?;
        } else {
            fs::copy(&source, &target).map_err(|e| format!("{}: {e}", target.display()))?;
        }
    }
    Ok(())
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

/// How many times the VM's userspace was soft-rebooted since it booted.
fn soft_reboots(agent: &mut Client<VsockStream>, deadline: Instant) -> Result<u32, String> {
    let (_, stdout, _) = run_shell(
        agent,
        argv(&[
            "systemctl",
            "show",
            "--value",
            "--property=SoftRebootsCount",
        ]),
        deadline,
    )?;
    stdout
        .trim()
        .parse()
        .map_err(|_| format!("systemd says it soft-rebooted {:?} times", stdout.trim()))
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

/// The UEFI firmware and the run's copy of its variables.
fn uefi_drives(firmware: &FirmwarePaths) -> [String; 4] {
    [
        "-drive".into(),
        "if=pflash,format=raw,unit=1,file=ovmf-vars.fd".into(),
        "-drive".into(),
        format!(
            "if=pflash,format=raw,unit=0,readonly=on,file={}",
            firmware.code.display()
        ),
    ]
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
        // What it said last, as a command that sends errors to stdout says them.
        let context = stderr
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .or_else(|| stdout.lines().map(str::trim).rfind(|l| !l.is_empty()));
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

/// What the console says when a guest won't come up by itself, so waiting longer is pointless:
/// the kernel panicked, or systemd gave up and waits in emergency mode for someone at the
/// console.
fn stuck(console: &Path) -> Option<&'static str> {
    let text = fs::read(console).ok()?;
    let text = String::from_utf8_lossy(&text);
    if text.contains("Kernel panic") {
        Some("the kernel panicked; see the console log")
    } else if text.contains("You are in emergency mode") {
        Some("it stopped in emergency mode; see the console log")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(text: &str) -> Result<VmSpec, toml::de::Error> {
        toml::from_str(text)
    }

    #[test]
    fn parses_spec_with_defaults() {
        let spec = spec("config = \"x\"\n").unwrap();

        assert_eq!(spec.config, "x");
        assert_eq!(spec.firmware, Firmware::Uefi);
        assert!(!spec.tpm);
        assert!(spec.wait_for.is_empty());
        assert!(spec.expect.is_empty());
        assert_eq!(spec.timeout_secs, 300);
    }

    #[test]
    fn wrath_is_the_only_store_a_scenario_may_name() {
        assert_eq!(
            spec("config = \"x\"\nstore = \"wrath\"\n").unwrap().config,
            "x"
        );

        for other in ["host", "image", "nfs"] {
            let err = spec_err(&format!("config = \"x\"\nstore = \"{other}\"\n"));
            assert!(err.contains(&format!("unknown variant `{other}`")), "{err}");
        }
    }

    #[test]
    fn a_scenario_needs_a_config() {
        assert!(spec_err("").contains("missing field `config`"));
        let err = spec_err("image = \"x\"\nconfig = \"y\"\n");
        assert!(err.contains("unknown field `image`"), "{err}");
    }

    #[test]
    fn only_a_disk_image_file_boots() {
        let dir = tempfile::tempdir().unwrap();
        let err = check_image(dir.path()).unwrap_err();
        assert!(err.ends_with("is not a disk image"), "{err}");

        let image = dir.path().join("system.img");
        fs::write(&image, "").unwrap();
        assert_eq!(check_image(&image), Ok(()));
    }

    #[test]
    fn a_system_in_wraths_store_is_copied_in_by_what_the_guest_lacks() {
        let have = "aaa\nbbb\n";
        let closure = ["aaa", "ccc", "bbb", "ddd"];
        assert_eq!(
            copy_command(have, &closure),
            Some("cp -a /run/wrath-store/ccc /run/wrath-store/ddd /wrath/store/".to_owned())
        );
        assert_eq!(copy_command("aaa\nccc\nbbb\nddd\n", &closure), None);
    }

    fn spec_err(text: &str) -> String {
        spec(text).unwrap_err().message().to_owned()
    }

    #[test]
    fn parses_wait_conditions_and_expectations() {
        let spec = spec(
            r#"
            config = "x"
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
            "config = \"x\"\nwait_for = [\"port:80\"]\n",
            "config = \"x\"\nwait_for = [\"unit:\"]\n",
        ] {
            let err = spec(bad).unwrap_err();
            assert!(err.message().starts_with("unknown wait condition"), "{err}");
        }

        let err = spec("config = \"x\"\nfirmware = \"bios\"\n").unwrap_err();
        assert!(err.message().contains("unknown variant `bios`"), "{err}");
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
              { apply_config = "fixtures/configs/basic", try_for = "20s" },
              { apply_host = "fixtures/configs/basic-changed" },
              { reboot = true },
              { run = "curl localhost", stdout_contains = "200" },
              { expect_no_reboot = true },
            ]
            "#,
        )
        .unwrap();

        assert_eq!(spec.config, "fixtures/configs/basic");
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
                    try_for: None,
                    exit: 0,
                },
                Step::ApplyConfig {
                    config: "fixtures/configs/basic".into(),
                    expect_plan: Vec::new(),
                    try_for: Some("20s".into()),
                    exit: 0,
                },
                Step::ApplyHost {
                    config: "fixtures/configs/basic-changed".into(),
                },
                Step::Reboot,
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
    fn a_command_can_run_as_a_user() {
        let spec = spec(
            "config = \"c\"\n\
             steps = [{ run_as = \"demo\", run = \"echo 'hi'\", stdout_contains = \"hi\" }]\n",
        )
        .unwrap();
        assert_eq!(
            spec.steps,
            [Step::Run(Expect {
                run: "su demo -c 'echo '\\''hi'\\'''".into(),
                exit: 0,
                stdout_contains: Some("hi".into()),
            })]
        );
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
        let err = spec("config = \"x\"\nwait_for = [\"user-unit:niri.service\"]\n").unwrap_err();
        assert!(err.message().starts_with("unknown wait condition"), "{err}");
    }
}
