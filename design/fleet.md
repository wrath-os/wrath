# Fleets

How many machines are run from one configuration repository: servers, appliances and
Kubernetes nodes, on rented hardware, in a datacenter or at home. It gathers what `vision.md`
(Fleets), `ideas.md` and decisions 0008, 0040 and 0043 say, plus lessons from running a
Kubernetes fleet on an immutable, API-only OS, into one design. Status: proposed, 2026-11-26.

## What it has to do

- Every machine runs a reviewed commit of the repository, and anyone can see which.
- A change rolls out in waves, stops on the first failure, and each machine that breaks puts
  itself back.
- As much as possible applies live. Reboots are rare, planned, and never two at once where it
  would cost quorum or a replica.
- A new machine goes from blank disk to serving its role with no shell and no hand-written
  config, and its identity survives install, rollback and reinstall.
- Losing the coordinator loses history and pacing, nothing else. Machines keep running, booting
  and upgrading on their own.
- A compromised coordinator or mesh can only move machines between reviewed, signed states.
- One machine and a thousand use the same model and the same verbs.

## Principles

1. **Git holds desired state.** Roles, machines, the lock and secrets are in the repository
   (0008). The coordinator holds only what git can't: what each machine runs, its facts, leases,
   approvals and how far a rollout got.
2. **Machines pull and check.** The coordinator says "go to commit X". The machine checks that X
   is on the repository it follows, builds or fetches it from caches it trusts, with its own keys,
   and switches itself. Nothing pushes a system onto a machine except a person with
   `apply --host`.
3. **The plan drives everything.** Each change already has a tier (live, next login, soft reboot,
   reboot). Rollouts pace by tier, approvals are per tier, and reboot slots exist only for the
   reboot tier.
4. **Live first.** The reboot tier is for what can't change under a running kernel: a new
   kernel, initrd or bootloader. Everything else restarts what changed (see Staying live).
5. **systemd does the work.** Units, restarts, `daemon-reexec`, soft reboot, boot counting,
   `boot-complete.target`, credentials, timers, the journal. wrath decides what changed; the
   agent is remote hands for the same code paths the CLI uses.
6. **Nothing at boot waits on the network.** Unlocking disks, booting and starting the system are
   local. The mesh and the coordinator matter only for changes.
7. **A small core.** The core gains a few contracts (JSON plans and status, health checks,
   operations, floors). The agent, the coordinator, providers and the discovery image are
   packages and roles in grapes, built and cached like anything else.

## The nouns

| Noun | What it is | Where it lives |
|---|---|---|
| Repository | Roles, `machines.toml`, the lock, secrets | git |
| Role | A kind of machine, built and locked once (0008) | `roles/NAME.toml` |
| Machine | A name, a role, machine values, an identity | `machines.toml`, the machine |
| Identity | One ed25519 key: SSH host key, secrets recipient, starmole node | the machine |
| Facts | Detected: SMBIOS UUID, CPU, memory, disks by path, NICs, TPM | the agent |
| Channel | A git ref machines follow: `prod`, `canary`, `test` | git |
| Rollout | One channel moving to a commit, wave by wave | coordinator |
| Wave | A set of machines moved together; the first is the canary | coordinator, from match rules |
| Health check | A unit a role declares; gates `boot-complete.target` and the next wave | role |
| Guard | A module's step before or after a disruptive change: drain, wait for quorum | module |
| Window | When reboot-tier changes may happen, per channel or role | repository |
| Operation | A resumable action outside generations: bootstrap etcd | the machine |
| Floor | The oldest generation a machine may go back to, after state moved forward | machine |

Channels are git refs on purpose. Moving `prod` to a commit is a reviewed push with history, and
a machine whose coordinator is gone can follow the ref directly, as 0040 already allows.

## Pieces and how they connect

```text
          git repository  ──────────── CI: wrath update, build every role, sign, push cache
         (roles, lock,                 compute each role's plan from channel to new commit
          channels = refs)                         │
               │                                   ▼
               │                           cache (bucket, 0038)
               │                                   ▲
   ┌───────────┴──────────┐                        │ fetch builds by recipe (0041)
   │      coordinator     │  wrath/agent/1         │
   │  SQLite: machines,   │◄──── starmole ────►  agent ── wrath (same code as the CLI)
   │  rollouts, leases,   │   (iroh, dials out,    │        systemd: units, boot counting,
   │  approvals, ops log  │    tags, revocation)   │        health checks, journal
   └──────┬───────────────┘                        │
          │ jobs                              one per machine
   ┌──────┴───────┐
   │  providers   │  Hetzner Robot / Cloud, PXE, Redfish, qemu: power, next boot, rescue
   └──────────────┘
```

### On each machine: wrath and the agent

wrath is unchanged in kind: it plans, switches, rolls back and boots. What the fleet adds to the
core:

- `plan --json` and `status --json` as a versioned contract. The agent and coordinator read
  nothing else, so an older agent understands a newer system as long as the contract holds.
- Health checks (0045): a role lists units that must pass. They gate `boot-complete.target`, so a
  generation that boots but can't serve rolls back by boot counting, and they gate the next wave.
- Operations as records in `/wrath/operations/`, plain JSON readable with `jq` from rescue media,
  one at a time under a lock, resumed by running the same command again.
- Module commands (0043) for operations a role brings: `wrath --host cp1 kube bootstrap`.
- Rollback floors: a module declares state that only moves forward (etcd after an upgrade, a
  Postgres major version). Past one, `rollback`, `try` and the boot menu refuse what's below it,
  and the plan says so.

The agent is a grapes package, a systemd service and a starmole node serving `wrath/agent/1`. It
dials out, so it works behind NAT with no ports open, and needs no tun and no capabilities. It
serves: status, facts, plan, apply, try, `--boot`, rollback, keep, reboot, soft reboot, logs
(the journal, streamed), kernel log, file reads under policy, module commands and operations.
Each call runs the same function the CLI does. An admin tag may also open a shell.

### The coordinator

A small service with SQLite, also a starmole node holding an issuer certificate. It mints
single-use invites limited to the tags it may give, tracks machines (key, role, tags, pending or
accepted, last status), runs rollouts, hands out reboot leases, records approvals and keeps an
operations log. Its database can be rebuilt from git plus a status call to every machine, apart
from history.

Machines with `[notify] by = "coordinator"` (0045) leave notifying to it: it sends their events
from the repository's `[notify]`, batched and routed by role.

It serves a page for approvals and the fleet view over the same API the CLI uses: machines by
role and channel, what each runs, rollouts in progress, what waits for a person and why.

### Providers

Separate processes, each holding its own credentials: Hetzner Robot, Hetzner Cloud, PXE/iPXE,
Redfish, qemu. They power machines on and off, set the next boot (network or disk), boot rescue,
and report what the provider knows. Facts have a common shape plus what each provider adds. The
coordinator calls them as jobs; it never holds their credentials.

### CI

On every commit: `wrath check`, build each role, push to the cache, sign. For each channel it
computes the plan from the channel's commit to the new one, per role, so a rollout knows its
tiers before it starts. A bot opens the `wrath update` pull requests. CI needs no fleet access.

### Caches

Buckets (0038) that machines trust by key. A fleet pulls from a cache CI has filled, so no
machine waits on a build at rollout time; `[updates] build = false` makes that a rule.

## Identity

A machine's identity is one ed25519 key: its SSH host key. `wrath secret machine` makes it
before install, secrets are encrypted to it, and when the machine joins starmole it uses the same
key as its node key, passed as the systemd credential `starmole.identity` and sealed by the TPM
with `LoadCredentialEncrypted` when there is one. The SSH fingerprint and the node id are one
public key. (starmole reads hex keys today and needs to read an OpenSSH private key too.)

- A blank-slate machine makes its key at first boot. Install carries it onto the disk, so the
  discovery system and the installed one are the same node.
- Rollback never touches it. A reset or reprovision chooses: keep the identity (same machine,
  new disk) or start over (a new machine that enrols again).
- An invite is spent once, at the first join. Every later connection uses the certificate. A
  join with a clock that's behind keeps the certificate and uses it once NTP sets the clock.
- Revoking the certificate cuts the machine off everywhere within seconds.
- One TLS identity per machine, the same before and after install.

The user docs say all of this: what's made when, what reset and rollback keep, what to do when a
board with the TPM is replaced.

### Secrets for many machines

Encrypting every secret to every machine means re-encrypting everything when one joins. Instead
each role gets a role key, kept in the repository encrypted to the role's machines and the
admins. A role's secrets are encrypted to the role key plus the admins. Accepting a machine
re-encrypts one small file, the role key, to one more recipient; that commit is made by CI
holding an admin key, or by a person approving. Removing a machine rotates the role key, which
re-encrypts that role's secrets. Per-machine secrets (its own keys, Secure Boot) stay encrypted
to the machine's own key.

## Following and channels

0040 already has machines follow a repository: `[updates] from`. A fleet adds a channel:

```toml
[updates]
from = "github:acme/fleet"
channel = "prod"          # a ref in that repository
switch = "auto"           # live changes now, the rest at the next window
```

With no coordinator, the machine follows the ref directly: each upgrade fetches the ref's commit
and applies it, as today. With a coordinator, the machine moves only when it holds a lease for
that commit, so a rollout paces it. If the coordinator is unreachable for longer than the role
allows, the machine keeps running what it runs; it doesn't race ahead to the ref.

## Flows

### Enrolling a machine

1. A provider boots the discovery image on the machine: a UKI from a USB stick, PXE, a cloud
   snapshot or rescue-and-dd. The image holds no secret, only the network's id. A single-use,
   short-lived invite arrives on the kernel command line, in a small config partition or in
   cloud metadata.
2. The discovery system makes the machine's key (TPM-sealed when there is one), joins starmole
   as `tag:pending` and reports facts. No rule matches `tag:pending`, so it can reach nothing.
3. Match rules in the repository pick role and placement from the facts: `role = "storage"`
   when there are two NVMe disks; datacenter, zone and rack from the provider. Disks are named
   by path from the facts, never picked by order.
4. Policy accepts, rejects or asks a person. Accepting commits `machines/NAME.toml` with its
   role and values, and adds the machine's key to its role key's recipients.
5. The coordinator retags the machine with its role's tag and tells it to install commit X. The
   agent runs `wrath install -C repo@X --role R`, carrying the identity onto the disk.
6. The machine reboots into its role. Its health checks include reaching the coordinator, so a
   generation that can't rolls back by boot counting. Once they pass, it's accepted.

### Rolling out a change

1. A pull request merges. CI builds every role, pushes to the cache and computes the plans.
2. Someone moves the channel (`prod`) to the commit, or a policy does for `test`.
3. The coordinator starts a rollout: waves from match rules, the canary first.
4. For each machine in a wave: the coordinator gives a lease for the commit. The agent fetches
   it, checks it's on the followed repository, fetches builds from the cache and applies:
   - Live and next-login changes switch now.
   - Soft-reboot and reboot changes are staged with `--boot`, and wait for a window and a reboot
     lease. Guards run first (drain the node, wait for etcd quorum), and leases are never given
     to two machines sharing a quorum or a replica set.
   - A change that touches the agent, starmole or networking is applied as a `try`, kept only
     once the agent has reconnected and the coordinator confirms.
5. Health checks pass, the machine reports, the next wave starts. A failure halts the rollout
   and the failed machine rolls itself back; the coordinator shows the plan, the failure and the
   journal around it.

What needs a person is set per channel and per tier: `test` applies everything on its own;
`prod` applies live changes on its own and waits for approval before soft reboot, reboot or any
change to disks.

### Going back

The usual way back is a revert commit rolled out like any change. In an emergency the
coordinator tells a wave to `rollback` to the generation it ran before; machines past a floor
refuse and say why. Recovery stops instead of looping: a failed trial boot falls back once, and
a bad fallback waits for a person.

### Operations

Some changes aren't a new generation: bootstrapping etcd, a Kubernetes minor upgrade, replacing
a disk, reprovisioning. The plan has two more outcomes beside the tiers, "needs an operation"
and "refused", and apply does nothing for those. Operations are module commands run through the
agent, one at a time per machine under a lock, recorded as JSON and resumed by running them
again. The coordinator sequences them across machines with the same leases and guards as
reboots.

Reprovisioning: the provider sets the next boot to the network, power-cycles, the discovery
image comes up keeping (or not) the identity, and enrolment runs from step 5.

### Debugging

The agent serves the journal, the kernel log, unit status and file reads, so diagnosing a
machine needs no new tool per failure. A shell over starmole is there for the admin tag and off
for others; servers can turn it off entirely. A second way in (management network, console,
BMC) stays possible, and the VM harness reaches a machine over vsock when its network is down.

## Staying live

The tiers decide how often a fleet reboots. To keep the reboot tier small:

- New packages, services, extensions and drivers are new store paths and started units: live.
- A kernel module built for the running kernel loads now. Kernel modules are packages built
  against the kernel, and a module that declares one has it loaded; only one built for a new
  kernel waits.
- Patches to userspace, libc and systemd included, restart the units whose closures reference
  what changed; systemd itself gets `daemon-reexec`. Soft reboot only when restarting everything
  is the honest answer.
- Runtime configuration (a container runtime's config, a service's options) restarts the
  service. No config needs a reboot just to be read.
- The plan says per change why it isn't live, so an unexpected reboot has a reason a person can
  read and a module author can fix.
- Reboot-tier changes wait as pending for a window; status shows when the running kernel and the
  booted-next one differ. Kernel live patches may later cover security fixes within one version.

## Refused by design

What the earlier fleet taught, as rules:

- A change that formats or retargets a disk is never applied from a diff. The plan refuses it,
  or it's an operation someone runs. Leaving a setting out never means "wipe".
- A generation records its whole system, so reinstalling or rolling back can't silently lose a
  kernel module or an image choice.
- Kernel options and hardening defaults (seccomp, user namespaces, nftables or iptables) are
  configuration, shown in the plan, chosen per platform or role.
- Merging configuration never appends duplicates to a list, and modules expose what needs
  changing as options, so no raw patch language creeps in.
- Services that hold quorum restart one machine at a time: a hostname change never restarts
  etcd on every control plane at once.
- No mode-dependent identities, no undocumented token behaviour, no metadata that survives a
  reset by surprise. The lifecycle is written down.
- Never a management API or installer unauthenticated by default, nor security that can only be
  chosen before install.

## Commands

Existing verbs, with targets, before any new command:

| Command | With a fleet |
|---|---|
| `wrath status --host SEL` | What each machine runs, its commit, pending reboots, last upgrade |
| `wrath plan --host SEL` | Plans per role for the selection, with the machines each covers |
| `wrath apply --host SEL` | From a checkout: push to the selection now (exists for one host) |
| `wrath plan --channel prod` | What moving `prod` to this commit does, per role and tier |
| `wrath apply --channel prod` | Move the channel and start the rollout, or approve a waiting wave |
| `wrath rollback --host SEL` | Roll the selection back, through leases and guards |
| `wrath --host SEL CMD` | A module command or operation on the selection |

`SEL` is a machine name, `role:NAME`, `channel:NAME`, a tag or `all`. Going through the agent
when there's a coordinator, and SSH otherwise. One new command is likely, for admitting
machines: `wrath machines` to list pending ones, accept, reject or reset. Everything else is a
target on a verb that exists.

## What a role configures

Nothing fleet-specific: a server's kernel, networks, firewall, access, storage, time and
updates are ordinary options (0046), shared through roles and profiles. What differs per machine
(addresses, gateways, rack, management address) is a machine value, allocated at enrolment and
pinned in the lock. Facts gain NICs by driver and PCI path, disks by path and the BMC model, so
match rules can pick a role from hardware and a role can match ports without naming them. BIOS
settings are applied through the BMC by a module command, with the same guards and windows as a
kernel update.

## Kubernetes nodes

A product on top, with nothing in the core that knows Kubernetes: recipes for containerd, runc
or crun, CNI plugins, etcd and Kubernetes from source; a kernel with what the CNI needs; drbd
and zfs as ordinary packages; a `kube` platform with no shell, PAM, getty or sudo (0030), and
`systemd-oomd` off. The cluster's PKI is role secrets. Bootstrap, upgrades, snapshots and
kubeconfigs are module commands. Guards drain a node before a reboot and wait for etcd quorum
before going on. A Kubernetes minor upgrade is an operation sequenced across the control plane.

## In the core, and outside it

Core, a few thousand lines at most: the JSON contracts, health checks, operations, floors, the
two extra plan outcomes, channels in `[updates]`, leases honoured by `apply --automatic`,
selectors for `--host`, and the agent transport for `--host`.

Outside, as packages and roles in grapes or their own repositories: the agent, the coordinator,
providers, the discovery role and image, match rules, Kubernetes modules.

## Order

1. On the machine: `plan --json` and `status --json` as a contract, health checks gating
   `boot-complete.target`, operations and module commands, floors.
2. The agent over starmole with the host key as identity; `--host` and selectors through it.
3. Channels and leases in `[updates]`; the coordinator with rollouts, waves, windows, guards and
   approvals; its page.
4. Enrolment: the discovery image, invites, match rules, role keys, one provider (Hetzner Robot),
   then PXE.
5. Kubernetes nodes.

Each step is useful alone: step 1 helps one machine, step 2 replaces SSH for `--host`, step 3
works for a handful of machines at home.

## Open

- Where approval lives: the coordinator's page, a separate policy service, or both.
- Who re-encrypts a role key when a machine joins: CI with an admin key, or a person approving.
- The new generation preparing its own boot: a small sandboxed program shipped by the new
  system, so an old agent never has to understand a new system's boot. Matters once agents and
  systems upgrade separately.
- How long a machine may go without a coordinator before it falls back to following the ref.
- The plan JSON's stability promise: what may change without a new version.
- Invites over plain PXE can be read on the wire; iPXE over HTTPS or a TPM quote checked at the
  exchange stops that.
- Multi-repository fleets (a platform team's base plus product repositories).
