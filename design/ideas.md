# Ideas

Ideas that belong to later milestones. Add a line here instead of building them now. The large
directions, and the products the core has to allow, are in `vision.md`.

- Project dev shells from a `wrath.toml`: packages, environment variables and services, using the
  same package names and lock as the system. Take devenv's ergonomics (direnv, processes,
  per-project services), so a project file names only packages, variables and services.
  Project services lower per platform: systemd-run user units on Linux, launchd agents on macOS,
  rc.d or daemon(8) on the BSDs, with a small supervisor built into wrath as the fallback.
- Machine policy: checks installed on a machine with its key (`wrath install --policy`), such as
  "the firewall is on", "only these ports" or "nothing in `kernel.sysctl` is forced". The machine
  checks every configuration it's given after merging, so nobody who writes the configuration can
  get around it, and errors point at the TOML line that broke the rule. Policy doesn't change how
  values merge. For fleets and immutable appliances; a single-owner machine doesn't need it.
- Settings changes under a second: change a theme in the TOML, save, and the editor or niri shows
  it right away. Evaluation already takes about 40 ms on the Framework config; the time goes
  around it. On 2026-11-12 a build of that config spent about 5.5 s before building anything
  (not yet explained, possibly hashing the checkout) and 0.7 s assembling the system. What it
  takes, for a change that only touches files:
  - Assembly does only what changed: the one rendered file's action runs, everything else is a
    cache hit, and putting the system together costs milliseconds.
  - A files-only switch. Today every switch runs sysusers, tmpfiles, `daemon-reload`, restarts and
    the user managers. When the plan says only files changed, moving `/etc/.wrath` is the whole
    switch.
  - Programs notice. A module says how its program picks up a new file: it watches it (niri,
    alacritty), a signal (helix on `SIGUSR1`), a command, or a restart. The plan shows which.
    Unknown: whether a program watching a path with inotify sees a change when only a symlink
    further along the chain moves. Test per program; a direct link or a touch may be needed.
  - No generation per save. `wrath try --watch` keeps one working generation and updates it in
    place on each save; `keep` makes it a generation and stopping goes back. Same verbs as today.
  - Per-user settings such as an editor theme wait for homes to come back. System-wide files
    (`/etc/xdg`, niri's system config) are enough to prove it.
  Changes to packages or units stay slower; that's fine.
- `wrath set services.nginx.port 8080`: changes a setting where the configuration lives. The
  value's origin, as `wrath why` finds it, says which file and line to edit; a value set nowhere
  goes in the machine's own file. TOML comments and layout are kept, and plan and apply follow as
  usual. A machine following a URL refuses, as `add` and `update` do, and shows the edit to make
  in the repository; later it could open a pull request there. No machine-local layer on top of
  a fetched configuration: the repository would stop describing the machine, two machines on one
  commit would differ, and every upgrade would have to merge it.
- `wrath try --set services.nginx.port=8080`: a generation from the configuration plus overrides,
  rolled back unless kept. The generation records them beside its origin. Keeping one for good
  means putting it in the configuration.
- `wrath verify`, in place of general drift detection: commands that change settings write the
  configuration, and `/etc` and the store are read-only, so what's left to check is small. It
  rehashes store paths against their names, which works on every platform, and lists files in
  `/etc` that nothing manages. To find out: what a hand-run `systemctl enable` does against the
  managed `/etc`. Which machine runs which commit is `status`'s job (0040).
- fs-verity on store paths for Linux machines, opt-in beside Secure Boot: the kernel checks each
  page as it's read, carrying the verified chain from the UKI into the store. Needs a filesystem
  that has it (btrfs, ext4); never something the design depends on, since macOS lacks it.
- macOS stores: a read-only APFS volume through `synthetic.conf`. arm64 macOS kills unsigned or
  wrongly signed executables, and rewriting store paths breaks a Mach-O signature, so the
  builder's fixup has to ad-hoc sign after rewriting before anything it builds runs there. The
  kernel checking those signatures per page covers executables; `verify` covers the rest.
- Servers that boot as a blank slate and wait for a control plane (Kubernetes nodes and other
  fleets; `vision.md` has the fleet design). One generic image, a UKI usable from a USB stick,
  PXE (iPXE over HTTP), a cloud snapshot or rescue-and-dd, boots a minimal system whose only
  job is to join starmole and serve the machine's API: facts (SMBIOS UUID, CPU, memory, disks by
  path, NICs, PCI devices, whether there's a TPM), then install and apply. The control plane
  sends a commit, the machine runs `wrath install` with it and reboots into the result.
  - Needs networking and userspace, so it's a booted system, not firmware; an initrd with
    networking might be enough, a small full system is simpler. In a datacenter, PXE.
  - Enrolling securely: the image holds no secret, only the network's id. What lets it join is a
    single-use, short-lived starmole invite, minted per machine or per boot by whatever
    provisions it, delivered on the kernel command line (PXE), in a small config partition
    (rescue-and-dd) or in cloud metadata. The machine makes its key at first
    boot, sealed by the TPM when there is one, and the control plane binds that key on first
    contact.
  - The same API stays once installed, so a node with no shell is a platform of wrath's with an
    agent, not a different design: the API makes the calls the CLI does. No A/B partitions
    either: the store and boot counting already give atomic updates and rollback.
  - A small declared identity state: the starmole key, the machine's identity and its enrolment
    record. Install carries it from the blank-slate system onto the disk, and rollback never
    touches it, so going back a generation never makes a machine a stranger to its fleet.
  - The agent's services are iroh ALPNs on its starmole node: the API, logs, maybe metrics. The
    journal stays on the machine; the log ALPN streams copies to a collector, so losing the
    collector loses no logs.
  - Not soft-bricking it when starmole or the agent changes: a change that touches starmole,
    the agent or networking is applied as a `try`, kept only once the agent has reconnected
    and the control plane confirms; the agent reaching the control plane is a health check
    gating `boot-complete.target`, so a generation that loses it rolls back by boot counting;
    starmole upgrades reach canaries first; and a second way in (a management network, the
    console, the BMC) stays possible.
  - Open: where the approval UI lives (the coordinator, a separate service, or a page over the
    API); and how a new machine's key joins its secrets' recipients (also open in `vision.md`).
  - What starmole already does for this: the agent and coordinator embed it as a library and
    serve `wrath/agent/1` with `Builder::serve`, checking the caller's tags through `Peers`.
    The coordinator holds an issuer certificate and mints per-machine invites limited to the
    tags it may give; an invite works once and runs out (`--expires 10m`). An agent that only
    serves ALPNs needs no tun and no capabilities. The machine dials out, the daemon opens
    the host firewall itself, and a relay covers the rest. `starmole check` can gate
    `boot-complete.target`, and revoking a certificate cuts a machine off everywhere.
  - Pending is a `tag:pending` set in the invite: the coordinator takes facts from it, and no
    rule anywhere matches it. Accepting the machine is `retag` (`starmole retag`, or
    `node.retag` from the library): the issuer re-signs its certificate with the role's tag,
    and every machine, that one included, has it within seconds.
  - Also built in starmole for this: a join with a clock that's behind says so, keeps the
    certificate and uses it once NTP sets the clock; the key comes from the systemd credential
    `starmole.identity` when the unit passes one; members forget machines whose certificates
    ran out, so live images that make a new key every boot don't pile up. Invites were
    already kept in the issuer's state file, so restarting the coordinator keeps them.
  - Still open in starmole:
    - An invite on the kernel command line over plain PXE can be read on the wire and redeemed
      first. It works only once, so the real machine failing shows it, but nothing stops it.
      iPXE over HTTPS helps; a `starmole-verify-tpm` verifier checking a TPM quote through the
      exchange stops it, as a separate program like `starmole-oidc`.
  - Wire compatibility, for a coordinator talking to live images built months earlier: the
    network policy now travels as the signed JSON bytes, so an older node passes a newer
    policy on whole and uses what it can read. A field old nodes mustn't ignore raises
    `needs_format`, and they keep the last policy they understood. Certificates and the
    other protocols still change only with a new ALPN or certificate version.
- Servers and Kubernetes nodes, beyond what the fleet design in `vision.md` covers. systemd
  already does most of what the machine needs; the store does what A/B slots, sysext and confext
  would, with more than one release of rollback.
  - Rollback floors. A module declares state that only moves forward: etcd after a control-plane
    upgrade, a Postgres major version, a Ceph release. Once a generation has crossed one,
    `rollback`, `try` and the boot menu refuse generations below it, and the plan says so.
  - The new generation prepares its own boot. The running wrath writes boot entries for every
    generation, so an old agent has to understand a new system. Instead the new system ships a
    small program, run sandboxed (no network, no devices, the ESP hidden), that makes its image
    and entry; the running side checks a small stable contract and moves the default. Matters
    once fleets upgrade agents and systems separately.
  - Two more plan outcomes beside the tiers: needs an operation (bootstrap, a Kubernetes upgrade:
    a module command, 0043) and refused. The plan says which, and apply does nothing for those.
  - Operations as records apart from generations: one at a time under a lock, resumed by running
    the same command again, kept as plain JSON in `/wrath` so rescue media can read them with
    `jq`. Generations say what runs; operations say how the machine got there.
  - Recovery that stops: a failed trial boot falls back once and the fallback doesn't re-arm, so
    a bad fallback waits for a person instead of looping. Check boot counting gives this.
  - Say what persists in user docs: `/var` and declared state never roll back, so rollback isn't
    a backup.
  - On the `kube` platform, `systemd-oomd` stays off with a check that fails the build if a
    module enables it: killing on memory pressure rather than free memory can take down an API
    server with plenty of memory left.
  - The VM harness reaches a machine over vsock when its network is down, beside the serial
    console.
  - Never a management API or installer unauthenticated by default, nor security that can only
    be chosen before install.
- Fleets reboot as little as possible. A reboot drains a node and takes it out for minutes, so
  as much as possible should apply at the live tier, and the reboot tier is kept for what can't
  change under a running kernel: a new kernel, the initrd, the bootloader. Today some changes
  land at soft reboot or reboot only because nobody worked out how to apply them live; each is
  worth a look.
  - Adding an extension, a driver, a tool or a service to a running machine is live: new store
    paths and units started. A kernel module built for the running kernel loads now; only one
    built for a new kernel waits for the reboot.
  - Patches to anything in userspace, libc and systemd included, should be live where they
    can be: restart the units that use what changed (the store says which reference it),
    `systemctl daemon-reexec` for systemd itself, and soft reboot only when restarting
    everything is the honest answer.
  - Lean on systemd for all of it: restarts and reloads, `daemon-reexec`, soft reboot,
    `systemd-run` for one-offs. wrath works out what changed and its tier, and systemd does
    the work, so there's no second service manager or extension mechanism to keep.
  - The plan says per change why it isn't live, so an unexpected reboot has a reason a person
    can read and a module author can fix.
  - A fleet applies live changes as they come and holds reboot-tier ones as pending until a
    maintenance window. The running kernel and the one the machine would boot can differ for a
    while, and status says so.
  - Kernel live patches could narrow even the kernel's share later, for security fixes within
    one kernel version.
- Lessons from running a Kubernetes fleet on an immutable, API-only OS. Worth keeping: the whole
  machine as reviewable documents (disks, modules, seccomp, firewall, kubelet); an authenticated
  API with the journal, kernel log, services and file reads, which diagnosed a lost cluster with
  no console; the image's identity content-addressed and readable back from the node; disks
  sealed through a KMS, harmless while it was down; idempotent bootstrap, a graceful reset that
  drains and leaves etcd, upgrades that keep state; a host firewall and hardened defaults that
  passed an outside probe; one generic image for every machine. What hurt, and what wrath does
  about it:
  - A field left out was an instruction. A rendered config without disk encryption reformatted
    the data volume on all three control planes and lost etcd and every replicated volume; one
    without the install image reinstalled a node without its storage modules; one whose disk
    pick drifted onto a replicated volume held every other change behind it. A change that
    formats or retargets a disk is never applied from a diff: the plan refuses it, or it's an
    operation someone runs. Disks are named by path, never picked by order.
  - Kernel features came only as extensions baked into an image: storage modules and a VM
    runtime each needed a new image, a reinstall or upgrade reboot, and the modules listed again
    by hand because the extension didn't load them. In wrath a kernel module is a package built
    against the kernel, and a module that declares one has it loaded (see the reboot bullet
    above for when it can load live).
  - Extensions sat badly in the lifecycle: new config for one applied only at the next boot,
    and a storage extension's export at shutdown, blocked by devices still holding its volumes,
    was killed after 10s and hung the node until a power cycle. Services are systemd units, so
    a config change restarts them, and shutdown ordering and timeouts are systemd's.
  - An in-place upgrade unmounted the running system and waited with no timeout; busy volumes
    hung it. Generations switch by boot entry or soft reboot, with nothing unmounted under a
    running system.
  - No shell: a pool-creating DaemonSet that entered the host found no `/bin/sh`, and each new
    failure needed a new debugging helper. The API stays, and a shell stays possible, off by
    default on servers if wanted.
  - A fixed kernel and defaults: nftables only, which moved the network plugin to pure eBPF and
    broke an add-on that expected kube-proxy's chains; a seccomp default that blocked user
    namespaces for VMs and sandboxes. Kernel options and hardening defaults are configuration,
    shown in the plan (see per-platform kernels below).
  - Patching config by merge appended duplicates to lists (a doubled namespace crashed the API
    server) and couldn't reach an interface chosen by a selector, so JSON patches crept in.
    TOML merges say what they do with lists, and modules expose what needs changing as options.
  - Behaviour learned only from source: join tokens re-redeemed every few seconds, so single-use
    tokens couldn't work; a per-machine token not persisted across boots; boot metadata
    surviving a reset; one port serving two TLS identities, self-signed before install and the
    cluster CA after, with nothing saying which. In wrath the identity is the machine's
    starmole key, made at first boot and carried by install, so the blank-slate system and the
    installed one answer as the same node, and the invite is spent once: later connections use
    the certificate. That lifecycle is written down for users, including what reset and
    rollback keep.
  - A hostname change pushed to all control planes at once restarted etcd on all three within
    30ms. Fleet rollouts know which services hold quorum and restart them one machine at a time.
  - Kubernetes and add-on upgrades, bootstrap and re-bootstrap were left to tooling outside the
    OS, about half of it. These are operations (above), with modules owning their steps.
  - Images built lazily on first request took 60-90s and timed out the first apply. A fleet
    pulls from a cache CI has already filled.
- Kernels per platform and per machine. Today there is one kernel recipe, with one set of options
  for VMs, the Framework Desktop and everything else; a configuration can load, blacklist and set
  options on modules, but not choose what's built. A platform, or a machine, could add or remove
  config options and modules as recipe options (`linux` built with `options.config`), each a
  build of its own kept by the cache: a desktop with every driver a PC might need, a server node
  with only its hardware's, no sound, Bluetooth or wifi, and lockdown and module signing on, so
  nothing loads that wasn't built with it. The plan shows a change of options as the kernel
  rebuilt, at the reboot tier.
