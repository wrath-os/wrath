# 0046: What a server is told

Status: proposed, 2026-11-27.

## Context

A server in a rack, or a node in a cluster, gets configured along the same lines wherever it
runs. A typical bare-metal node needs:

- Kernel. A pinned kernel with the previous one kept for going back, extra command line
  arguments (`amd_pstate=active`, `cpufreq.default_governor=performance`), modules loaded at
  boot (`rbd`, `vrf`), modules that must never load (`esp4`, `rxrpc`, `algif_aead`, a GPU's host
  driver when the GPU is passed through), out-of-tree modules built for the kernel, and sysctls,
  some of them per interface (`net.ipv4.conf.<if>.rp_filter`).
- Network. Bonds (802.3ad with a fast LACP rate and layer3+4 hashing, or active-backup), VLANs
  on the bond, bridges per VLAN for virtual machines, a VRF with its own routing table, extra
  addresses on `lo` for things announced over BGP, static IPv6 addresses with router
  advertisements off, routes and policy routing rules, jumbo MTUs, NIC ring buffer sizes, and
  `/etc/hosts` entries.
- Firewall. Ports per interface or per network, port ranges (`30000-32767`), accepted source
  prefixes, forwarding allowed between named interfaces, TCP MSS clamping on egress interfaces,
  and an early filter that drops spoofed sources on an external interface. All of this next to
  netfilter rules that a container runtime and a CNI add on their own.
- Access. Admin users with exact authorized keys and passwordless sudo, an SSH user CA so
  certificates replace keys, root login by certificate only, sshd listening only on a
  management network's address, and system users for daemons.
- Devices and groups. A group with a fixed gid (`tss`, so pods get a known supplemental group),
  device nodes with a group and mode (`/dev/tpmrm0`), and udev rules.
- Storage. A local volume of a fixed size carved from a volume group, LVM's devices file so the
  host never activates volumes found inside guests' disks, mdraid, NFS mounts with options such
  as `nconnect=8`.
- Time and audit. NTP servers, or the network's; an audit ruleset; journal limits.
- Updates. Security updates applied on their own, the kernel held back for a reboot window, a
  node drained before a reboot, a "reboot needed" signal for the scheduler.
- Firmware. BIOS settings (CPU power states, fixed fabric clocks) changed through the BMC's
  Redfish API, then a reboot.

wrath covers some of this today. `kernel.modules`, `kernel.blacklist`, `kernel.module_options`,
`kernel.sysctl` and `boot.kernel_params` exist; so do `networking.networks` with a `sections`
escape hatch, a firewall with a port list, `time.servers`, users with authorized keys, and
`disks` for partitions and filesystems. Bonds, VLANs, bridges, VRFs, routes, port ranges,
per-interface rules, forwarding, an SSH CA, fixed gids, device permissions, LVM volumes and
mounts aren't options yet. Through `sections`, `files` and `units` they can be written by hand,
which is how every server ends up with its own copy of the same twenty lines.

Most of this applies to any machine wrath runs, a home router included. Nothing here is specific
to fleets. A fleet adds roles to share it and machine values to fill in what differs.

## Decision

These become ordinary options in the modules that already own the area, written in TOML like
any other setting, with no new concepts. Where a role is shared by machines that differ (an
address, a rack number), the value is a machine value (0008). Where hardware differs in naming
(NIC names), the role matches by property instead of name. Every option says its tier.

### Rules for all of it

- A module owns each area: `kernel`, `network`, `firewall`, `ssh`, `accounts`, `disks`. New
  options extend those modules; no "server" module duplicates them.
- Sets of settings that go together are profiles, included by roles (0007): a
  `profiles/cluster-node.toml` with the inotify and aio sysctls, a `profiles/fast-network.toml`
  with buffer sizes and `fq`. No option bundles sysctls under a name.
- What differs per machine is a machine value: `${machine.address}`, `${machine.rack}`. Options
  that may hold one are `runtime` options (filled in at switch, as `files.*.text` is), so every
  machine of a role runs the same build. Addresses can be allocated and pinned in the lock, as
  uids are (0029).
- Hardware is matched by what it is. A bond's members are the ports driven by `mlx5_core`, or at
  a PCI path, so two servers whose ports are named `enp65s0f0np0` and `enp1s0f0np0` share a role.
- Sets merge without duplicates. A role can add a port to what a profile opened, and nothing
  appends a second copy.
- Raw text stays possible where a format has no good typed model (nftables, audit rules), as a
  whole file or table of its own. It never patches what a module wrote.

### Kernel

```toml
[kernel]
package = "linux-6.18"          # a series; the lock pins the exact build
modules = ["rbd", "vrf"]
deny = ["esp4", "esp6", "ipcomp4", "ipcomp6", "rxrpc", "algif_aead"]
extra_modules = ["jool"]        # out-of-tree, built against this kernel

[kernel.sysctl]
"fs.inotify.max_user_watches" = 524288
"net.core.default_qdisc" = "fq"

[boot]
kernel_params = ["amd_pstate=active", "cpufreq.default_governor=performance"]
```

- `package` names a series. The lock pins the build, so an upgrade is `wrath update` and a
  reviewed lock change. The previous kernel is the previous generation's, kept for rollback and
  in the boot menu; nothing else keeps kernels side by side.
- `deny` writes `install <m> /bin/false`, which also stops an explicit `modprobe`, and puts the
  module out of the initrd. `blacklist` stays for "don't autoload". At apply, a denied module
  that's loaded and unused is unloaded (live); one in use is the reboot tier, and the plan says
  which.
- `extra_modules` are recipes built against the configured kernel, so a new kernel gets them
  rebuilt with it and a module that doesn't build fails the build, before any reboot. Modules
  declaring one get it loaded (`fleet.md`, Staying live). No DKMS on the machine.
- Per-interface sysctls belong to the interface, so they're network options (below). Kernel
  sysctls stay in `kernel.sysctl`, live.
- `boot.kernel_params` and initrd changes are the reboot tier.

### Network

Networks stay as they are. Virtual devices, links, addresses and routes join them, rendered to
systemd-networkd's `.netdev`, `.link` and `.network` files:

```toml
[networking.links.data]
match = { driver = "mlx5_core" }      # or path = "pci-0000:41:00.*", or names
mtu = 9100
ring_buffers = { rx = 8192, tx = 8192 }

[networking.devices.bond0]
kind = "bond"
members = ["data"]                    # links by name
bond = { mode = "802.3ad", lacp_rate = "fast", hash = "layer3+4", miimon = "100ms" }
mtu = 9100

[networking.devices.internal]
kind = "vlan"
on = "bond0"
vlan = 121

[networking.devices.internet]
kind = "vrf"
table = 1001

[networking.devices.br120]
kind = "bridge"
on = "bond0"
vlan = 120                            # makes the VLAN and puts it in the bridge

[networking.networks.internal]
match = ["internal"]
dhcp = "no"
accept_ra = false
addresses = ["${machine.address}/64"]
routes = [{ to = "2001:db8::/32", via = "${machine.gateway}" }]

[networking.networks.external]
match = ["bond0.130"]
vrf = "internet"
rp_filter = "off"
routes = [{ to = "::/0", via = "${machine.external_gateway}", table = "internet" }]

[networking.loopback]
addresses = ["fd00:0:0:1::2000/128", "fd00:0:0:1::2001/128"]

[networking.hosts]
"fd00:0:0:1::2000" = ["api.cluster.internal"]
```

- `links` are `.link` files: matching by driver, path, MAC or name, MTU, ring buffers, offloads.
  udev applies them when the device appears, so no oneshot service runs `ethtool` after boot.
- `devices` are `.netdev` files of a `kind`: `bond`, `vlan`, `bridge`, `vrf`, `dummy`,
  `wireguard`, `vxlan`. Each kind's settings are a record of the few that matter, plus
  `sections` for the rest.
- `networks` gain `addresses`, `routes`, `rules` (policy routing), `vrf`, `bridge`, `rp_filter`,
  `forwarding` and `mtu`. Addresses and gateways take machine values. `rp_filter` and other
  per-interface sysctls are written as networkd settings where networkd has them, and as
  `net.*.conf.<name>.*` sysctls otherwise, applied when the interface appears.
- Servers keep an interface's addresses when networkd restarts (`KeepConfiguration`), so a
  restart never drops the address someone is connected over.
- A change to a network is live: networkd reloads and reconfigures what changed. A change that
  could cut off the machine's management path is applied as a `try`, kept once the machine is
  reachable again.
- `networking.hosts` writes `/etc/hosts` lines; the hostname line is always there.
- A VRF's module is loaded before networkd needs it, since `kernel.modules` and networkd both
  come from the same generation; there's no two-pass apply.

### Firewall

Zones group interfaces or sources, and rules are per zone. The flat options stay as a zone
called `default` that covers every interface:

```toml
[networking.firewall.zones.internal]
interfaces = ["internal", "br*"]
tcp = [22, 2379, 2380, 6443, "30000-32767"]
udp = [53, 8472]
from = ["2001:db8:100::/56"]          # optional; otherwise any source on these interfaces

[networking.firewall.zones.external]
interfaces = ["bond0.130"]
tcp = [80, 443]
drop_spoofed = true                   # private, documentation and bogon sources dropped early

[networking.firewall]
forward = [["internal", "external"]]  # zones or interfaces allowed to forward between them
clamp_mss = [{ out = "bond0.130", mss = 1460 }, { out = "bond0.130", mss = 1440, family = "ipv6" }]

[networking.firewall.tables]
"inet lb-prefilter" = { file = "nft/lb-prefilter.nft" }
```

- Ports are a number or a range. Ranges are a type of their own (`types.ports`), checked and
  merged like ports.
- `drop_spoofed` is a prerouting chain at a priority before conntrack, from a list the firewall
  module keeps, per address family.
- `forward` adds a forward chain with only those pairs accepted; without it the module writes no
  forward chain, so whatever a container runtime or CNI sets up for forwarding keeps working.
- `tables` loads whole nftables tables of your own from files next to the configuration, in the
  same transaction as the module's table. The module never edits them and they never edit it.
- wrath's tables live beside other tables. iptables-nft and nftables rules from other programs
  are left alone, as tailscale's are today.
- All of it is live: a new ruleset replaces the old in one step.

### Access

```toml
[services.ssh]
listen = ["${machine.mgmt_address}"]  # socket-activated, bound to these only
user_ca = ["ssh-ed25519 AAAA... ops-ca"]
root_login = "certificate"            # "no", "key", "certificate"

[users.ops]
extra_groups = ["wheel"]
authorized_keys = ["ssh-ed25519 AAAA... ops@laptop"]
sudo = "nopassword"

[groups.tss]
gid = 100108
```

- `user_ca` trusts certificates signed by these keys (`TrustedUserCAKeys`), and principals can
  be limited per user. The CA's public key is in the configuration, never fetched at boot.
- `root_login = "certificate"` allows root only with a certificate.
- `listen` binds sshd's socket to addresses that may only appear later (`FreeBind`). An empty
  list means every address.
- Normal users not in the configuration are removed at apply, keeping their uid reserved in the
  lock. `authorized_keys` is exact.
- `sudo = "nopassword"` per user or per group. The default stays a password.
- `groups.NAME.gid` pins a gid. Otherwise the lock keeps the gid it chose (0029). A pinned gid
  that collides with a locked one is an error at the TOML line.

### Devices

```toml
[hardware.devices.tpm]
match = { subsystem = "tpmrm" }
group = "tss"
mode = "0660"

[hardware.udev]
rules = ['KERNEL=="nvme*", GROUP="disk-exporter", MODE="0660"']
```

`hardware.devices` is the common case, rendered as a udev rule; `hardware.udev.rules` is the
rest. Both live, by reloading udev and triggering the matching subsystem.

### Storage

`disks` lays out disks at install. It gains LVM, mdraid and volumes, and mounts become their
own option for what isn't on a local disk:

```toml
[disks.main.partitions.lvm]
size = "rest"
lvm = "system"                        # a physical volume in volume group `system`

[disks.volumes.local-path]
group = "system"
size = "400G"
format = "ext4"
mount = "/var/lib/local-path"

[disks.lvm]
devices_file = true                   # only the volume groups listed here are activated

[mounts."/mnt/shared"]
what = "nfs.example.internal:/shared"
type = "nfs"
options = ["nconnect=8", "vers=4.2"]
```

- A new volume in a group with space is created at apply (live). Growing one is live. Shrinking,
  reformatting or moving one is refused and becomes an operation (`fleet.md`, Refused by
  design), on any machine.
- `devices_file` writes LVM's devices file with the configured physical volumes, so volumes
  inside guests' disks or Ceph's are never activated. The initrd gets the same file.
- `mounts` are mount units. The mountpoint is made immutable while empty, so nothing writes into
  it when the mount is missing, and services can say they need it (`RequiresMountsFor`).
- `raid = "raid1"` exists for btrfs; mdraid is `disks.NAME.raid` with a level, for filesystems
  that need it.

### Time, audit and the journal

```toml
[time]
servers = ["ntp1.example.internal", "ntp2.example.internal"]
serve = ["2001:db8:100::/56"]         # answer NTP for these clients

[security.audit]
rules = { file = "audit/rules" }

[journal]
max_use = "2G"
forward = "syslog.example.internal:6514"
```

- `time.serve` switches from timesyncd to chrony, which can serve; otherwise timesyncd.
- The audit module installs auditd with the rules as given, plus `log_format` and rotation.
  A ruleset is a file because the audit language is its own.
- `journal` covers size, persistence and forwarding.

### Updates and reboots

0040's `[updates]` covers this, with tiers doing what package exclude lists do elsewhere:

```toml
[updates]
from = "github:example/servers"
automatic = "daily"
switch = "auto"
reboot_window = "Tue,Thu 02:00..04:00"   # new: when a staged generation may reboot
```

- An update whose changes are all live, a library or service patch, switches now and restarts
  the units using it.
- An update that includes a kernel is staged whole with `--boot`, as `auto` does today, and the
  machine reboots into it within `reboot_window`. Without a window, wrath never reboots by
  itself (0040). Holding the kernel at one build while the rest updates needs
  pinning one package's version (still to come, `cli.md`), and keeps routine updates live.
- `status --json` says a reboot is pending and why, which is what a reboot scheduler reads.
- Draining a node before a reboot is a module's guard (`fleet.md`): the Kubernetes module drains
  and uncordons, others can add their own. Alone, a machine runs guards before its own reboot;
  in a fleet, the coordinator also hands out reboot leases.
- After the reboot, health checks (0045) decide whether the new kernel stays.

### Firmware and BIOS settings

The BIOS isn't the system, so it isn't a generation. A role says what it expects, and a module
command applies it through the BMC:

```toml
[hardware.firmware_settings]
redfish = { "ApbDis" = "1", "DfCStates" = "Disabled" }
```

- `wrath --host NAME firmware apply` (a module command, 0043) stages the settings over Redfish
  with credentials from secrets, waits for the BMC, and asks for a reboot through the same guards
  and window as a kernel update.
- `wrath status` shows when the BMC reports values different from the role's. Values the BMC
  doesn't know are reported, never guessed.
- Models that can't take the settings are a role of their own, or the role leaves the table
  empty for them.

### What a fleet adds

Nothing above needs a fleet. With one:

- Roles carry the settings, profiles hold what's shared, and machine values hold addresses,
  gateways, rack numbers and management addresses. An address pool allocates them at enrolment
  and pins them in the lock.
- Match rules pick a role from facts. NICs by driver and PCI path, GPUs, disks and BMC model
  become facts, so `role = "gpu-passthrough"` can follow from the hardware.
- Everything the API does is a commit: changing a node's settings is changing its role or its
  machine values, reviewed and rolled out as any change. The coordinator never sets a sysctl.
- `plan --json` shows each change's tier, so a rollout knows a firewall change is live and a
  kernel parameter waits for a window.

## Consequences

- A server's whole OS layer is TOML in one repository, with errors at the line and a plan with
  tiers, instead of templates rendered per host.
- Interchangeable hardware with different NIC names, or a role that differs only by address,
  shares one build.
- The `network`, `firewall`, `ssh`, `accounts`, `kernel` and `disks` modules grow, and so do
  their tests. Most of it is rendering systemd's own formats.
- Raw nftables tables and audit rules remain as files; they can't be checked beyond syntax.

## In the core, and in modules

Core: `types.ports` for ranges, facts for NICs (driver, path, MAC, speed), disks by path and the
BMC model, and a check that a pinned gid doesn't collide. A few hundred lines.

Everything else is grapes: options and rendering in the modules named above, `extra_modules` as
recipes against the kernel, the firmware command, a guard for draining.

## Order

1. Network: `links`, `devices`, `addresses`, `routes`, `rules`, `vrf`, `loopback`, `hosts`; a
   VM scenario with a bond of two virtio NICs, a VLAN and a VRF.
2. Firewall zones, ranges, forwarding, MSS clamping, spoof filter, own tables.
3. `kernel.deny`, `extra_modules`, per-interface sysctls.
4. SSH CA, `listen`, `root_login`, `sudo`, pinned gids, devices.
5. LVM volumes, devices file, mounts, mdraid.
6. chrony, audit, journal; NIC and BMC facts; the firmware command.

## Rejected

- A separate network language that renders to networkd. networkd's own model, typed where it
  matters and with `sections` for the rest, is one layer less.
- A role per NIC naming scheme. Matching by driver and path does it.
- Package-name exclude lists for automatic updates. The tier says what waits.
- Appending to another program's firewall rules files. Separate tables do the same with no
  ordering surprises.
- Fetching an SSH CA's key from a URL at boot. Boot doesn't wait on the network (`fleet.md`).
- Named sysctl bundles as options. Profiles are includes.

## Open

- How to express "the management path" so a network change that would cut it is tried rather
  than applied.
- Whether `drop_spoofed`'s lists come from a package that updates them, or stay fixed in the
  module.
- How much of each `.netdev` kind to type; the rest stays in `sections`.
- Whether firmware settings can be read back from every BMC well enough to report drift.
