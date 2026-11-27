# 0045: What a good boot means, and what happens after a bad one

Status: proposed, 2026-11-27.

## Context

Every new generation gets a boot entry with three tries (`wrath-N+3.conf`). The boot loader
counts a try each time it starts the entry; reaching `boot-complete.target` makes
`systemd-bless-boot` mark it good; after three failed boots the entry sorts last and the boot
loader starts the newest other generation, every boot from then on. `scenarios/native-tpm.toml`
checks that.

What that doesn't cover:

- A good boot only means userspace came up. Nothing gates `boot-complete.target`, so a router
  whose kernel boots but whose network or DNS is broken is marked good, and stays broken.
- A hang never uses up a try. A machine stuck in the initrd, or after a panic that doesn't
  reboot, waits for someone to power-cycle it. On a router or a rented server nobody is there.
- After a fallback, `status` says the failed generation "boots next", since it only compares the
  running generation with the current one. It doesn't, and nothing else says what happened.
- Nobody is told. A machine can sit on an older generation for weeks.
- `wrath try` protects changes to a running system. Nothing protects a change that only shows at
  the next boot.

What's good enough differs per machine. A desktop is fine once it reaches a login screen. A
router isn't fine until it routes and resolves. A server isn't fine until its services answer,
and a Kubernetes node until it has joined. So the conditions are configuration, and modules can
offer checks for what they set up.

## Decision

A boot is good when the machine reaches `boot-complete.target`, and the configuration says what
that takes. A boot that doesn't get there within a time limit, or hangs, or panics, uses up a try,
so a bad generation always ends with the machine on the last good one, with nobody at the
console. When that happens the machine says so, and anything can be hooked on to tell someone.

### Saying what a good boot takes

```toml
[boot.health]
checks = ["systemd-networkd-wait-online.service", "unbound.service"]
ping = ["1.1.1.1"]
resolve = ["example.com"]
no_failed_units = true
within = "10min"
```

- `checks` lists units that must succeed. Each gets `RequiredBy=` and `Before=` on
  `boot-complete.target`, so a failure keeps the boot from counting as good.
- `ping`, `resolve`, `tcp` (`"host:port"`) and `http` (a URL answering 2xx) are checks the
  health module writes as small units, each retrying until it passes or `within` runs out.
  Anything else is a unit of your own named in `checks`.
- `no_failed_units` enables `systemd-boot-check-no-failures.service`: any failed unit fails the
  boot. Strict, so off by default.
- `within` is how long the machine has to reach `boot-complete.target`. Past it, the target's
  `JobTimeoutAction=reboot-force` reboots, which uses up a try. Default 15 minutes, long enough
  for a slow disk unlock at a console.
- Modules offer checks for what they run. A module with something worth checking adds an option
  such as `services.unbound.boot_health = true`, which adds its unit, or a better check of its
  own, to `checks`. Off by default, so no service gates booting unless the configuration asks.
- The plan shows what gates booting, and a change to it is a change at the next boot.

### Hangs and panics

- `boot.watchdog = "30s"` sets systemd's `RuntimeWatchdogSec`, so a hung machine with a hardware
  watchdog is reset. On by default where the platform knows there's one (servers, routers), off
  on desktops and laptops.
- `boot.reboot_after_panic = "10s"` puts `panic=10` on the kernel command line. On by default.
- A watchdog in the initrd covers a hang before the system's own systemd starts, since the
  initrd runs systemd too.

### How many tries

`boot.tries = 3`, the count new entries get. A router may want 1: one bad boot and it's back.
The count is in the entry's name, so changing it applies to entries written from then on.

### After a fallback

A unit early in each boot, from the health module, compares the generation that booted with the
current one. When the current one's entry is out of tries and another runs, it:

1. Runs `wrath rollback M` for the running generation M, so the current generation, `status`,
   `rollback` and the next `apply` all agree on what runs. The failed generation keeps its entry
   (out of tries) and its journal, for looking at with `journalctl -b -1`.
2. Leaves a record of it: `status` says "generation N didn't boot after 3 tries; M runs", and
   `status --json` has it as `boot_failed`. It's also written to
   `/wrath/system/notice.d/boot`, which console and SSH logins show.
3. Starts `wrath-boot-failed.target`.

### Telling someone

Who gets told, how, and by whom is configuration, and covers more than boots. A failed boot is
one event; a failed automatic upgrade (0040) and a reboot waiting for its window are others.
Each is a target (`wrath-boot-failed.target`, `wrath-upgrade-failed.target`) and a record in
`status --json`, so anything can react to it.

```toml
[notify]
events = ["boot_failed", "upgrade_failed"]   # default: every event
by = "machine"                               # or "coordinator", or "none"

[notify.to.ops]
kind = "webhook"
url = { secret = "ops-webhook" }

[notify.to.me]
kind = "command"
command = ["my-pager", "--urgent"]           # gets the event as JSON on stdin
```

- `kind` is a method grapes knows (`webhook`, `email`, `command`, and push services as they're
  asked for) and `command` covers the rest: any program, given the event as JSON. A unit of
  your own wanted by an event's target works too.
- `by = "machine"` sends from the machine. `by = "coordinator"` sends nothing locally: the agent
  reports the event and the coordinator notifies, from the same `[notify]` shape in the
  repository, so a fleet can batch, deduplicate and route by role. A machine whose coordinator
  is unreachable for longer than its role allows falls back to its own targets, if it has any.
- Without `[notify]`, the record in `status` and the login notice are all there is.

In a fleet the agent reports the event either way, and a rollout halts on a boot failure
(`fleet.md`).

`wrath apply` of the failed generation's configuration again gives it fresh tries, as today. So
does `wrath rollback N` to it.

### Trying a change at the next boot

`wrath try --boot` makes the new generation boot once, through the boot loader's one-shot entry
(`bootctl set-oneshot`), without making it the default. If it reaches `boot-complete.target`,
`wrath keep` makes it the default; the machine can be told to keep it on its own once its checks
pass (`try --boot --keep-if-healthy`). If it doesn't, the next boot starts the default again,
with no tries used up. For changes to a remote machine that only show after a reboot: a kernel,
the initrd, network config read at boot.

### In the core, and in modules

Rust, because it reads or writes generations, boot entries or the notice: fallback in `status`
and `status --json`, the notice record, `boot.tries` in the entries wrath writes, and
`try --boot`. About 150 lines.

Modules in grapes: the `boot.health` options and their units, the check units, the watchdog and
panic options, the fallback unit (a script declaring `wrath` and `jq`, per 0043), and `[notify]`
with its methods.

## Consequences

- A broken generation ends on the last good one by itself, with the conditions for "broken"
  chosen per machine and role.
- The fleet's health checks (`fleet.md`) are these: a rollout waits for them and halts on a
  failure, and nothing new is needed for one machine to have them.
- A strict configuration can turn a working machine into one that falls back, for instance a
  ping to a host that's down. Checks are off unless asked for, the plan shows them, and the
  notice says which check failed.
- A machine whose clock or network is slow at boot needs `within` set to match.

## Order

1. `status` and `status --json` report a fallback, with the misleading "boots next" fixed. The
   notice record.
2. The health module: `checks`, `within`, `no_failed_units`, the fallback unit and its target.
   A VM scenario where a generation with a failing check falls back after its tries, the
   running generation becomes current, and the notice shows at an SSH login.
3. `ping`, `resolve`, `tcp`, `http`; `boot.tries`; watchdog and panic; `[notify]` with
   `webhook` and `command`.
4. `try --boot`.

## Rejected

- A health daemon of wrath's own. systemd already orders units, times out targets, reboots on a
  timeout and blesses boots.
- Making a failed generation's checks run again on the next boot without a reboot. A boot is the
  unit of trust here; a check that failed once at boot says that boot is bad.
- Failing a boot on any failed unit by default. On a desktop, a printer service failing would
  send it back a generation.

## Open

- A soft reboot doesn't pass through the boot loader, so it doesn't count tries. A generation
  switched to by soft reboot could arm the same checks and switch back on failure, as `try` does.
- Which hardware watchdogs the platforms know about, and whether a fact can say there's one.
- Whether a check can say why it failed better than its unit's status, for the notice.

## Built so far

- `status` and `status --json` report a fallback (`boot_failed`), with `boots_next` saying what
  the boot loader starts. `rollback` from a failed boot keeps a record until a newer generation is
  made or the failed one is rolled back to. Logins show `/wrath/system/notice.d`, a file per
  subject, on every machine. `status --json` prints the machine's part without a configuration.
- grapes' `boot-health` module: `checks`, `ping`, `resolve`, `tcp`, `http`, `within`,
  `no_failed_units`, `boot.watchdog`, `boot.reboot_after_panic` (in `base`), the fallback unit
  and `wrath-boot-failed.target`. Checks gate `boot-complete.target` through a drop-in, since
  units can't say `RequiredBy=` yet. A failed check reboots only on a boot the boot loader counts.
- grapes' `notify` module: `webhook` and `command`, for `boot_failed` and `upgrade_failed`
  (`wrath-upgrade-failed.target`, from the upgrade unit's `OnFailure=`). `by = "coordinator"`
  sends nothing locally until there is a coordinator.
- `scenarios/native-boot-health.toml`: a generation whose check fails falls back, is recorded,
  notified and shown at login. The harness's `until` step waits through reboots the VM does on
  its own.
- Not yet: `boot.tries`, `try --boot`, module options such as `services.unbound.boot_health`.
