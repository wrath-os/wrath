# A machine's disks: their partitions, which are encrypted, and the filesystems on them, laid out
# by `wrath install`. The booted system unlocks and mounts them by name: each partition is
# labelled `<disk><n>-<partition>`, such as `nvme0-system`, and an encrypted one opens under the
# same name.

device = types.record(
    count = option(types.int, default = 1, doc = "How many such disks the machine has."),
    kind = option(types.one_of(["nvme", "sata", "virtio", "any"]), default = "any"),
    at_least = option(types.str, optional = True, doc = "The smallest size, such as `500G`."),
    paths = option(
        types.list(types.str),
        doc = "The disks themselves, such as `/dev/disk/by-id/...`, for a machine that isn't a " +
              "role. Instead of `count`, `kind` and `at_least`.",
    ),
)

partition = types.record(
    size = option(types.str, doc = "Such as `1G`, or `rest` for what's left on the disk."),
    format = option(
        types.one_of(["vfat", "ext4", "xfs", "btrfs", "swap"]),
        optional = True,
        doc = "A filesystem on just this partition. One on several is under `filesystems`.",
    ),
    mount = option(types.path, optional = True),
    options = option(types.list(types.str)),
    encrypt = option(types.bool, default = False, doc = "Encrypted with LUKS."),
    on = option(
        types.one_of(["all", "first"]),
        default = "all",
        doc = "Whether every one of the disks gets the partition, or only the first.",
    ),
)

disk = types.record(
    devices = option(device, default = {}),
    partitions = option(
        types.map(partition),
        doc = "By name. Fixed sizes come first, by name, and the `rest` last.",
    ),
)

subvolume = types.record(
    mount = option(types.path, optional = True),
    options = option(types.list(types.str)),
)

filesystem = types.record(
    format = option(types.one_of(["btrfs"]), default = "btrfs"),
    raid = option(types.one_of(["single", "raid1"]), default = "single"),
    on = option(
        types.list(types.str),
        required = True,
        doc = "Partitions as `<disk>.<partition>`, on every disk that has them.",
    ),
    mount = option(types.path, optional = True),
    options = option(types.list(types.str)),
    subvolumes = option(types.map(subvolume)),
    degraded = option(
        types.one_of(["boot", "stop"]),
        default = "boot",
        doc = "With a disk of a RAID missing, boot from the others and say so, or stop until " +
              "someone boots it by hand.",
    ),
)

ssh = types.record(
    port = option(types.int, default = 2222),
    authorized_keys = option(
        types.list(types.str),
        doc = "Public keys allowed to log in to the initrd and unlock the disks. Without any, " +
              "the initrd has no SSH.",
    ),
)

unlock = types.record(
    tpm2 = option(
        types.bool,
        default = False,
        doc = "Enroll the machine's TPM at install, so its disks unlock without a passphrase " +
              "while the PCRs in `pcrs` stay the same.",
    ),
    pcrs = option(
        types.list(types.int),
        default = [7],
        doc = "The TPM measurements unlocking depends on; 7 is the Secure Boot state.",
    ),
    ssh = option(
        ssh,
        default = {},
        doc = "Unlock over SSH in the initrd, with the machine's initrd key.",
    ),
)

UNITS = {"K": 1024, "M": 1024 * 1024, "G": 1024 * 1024 * 1024, "T": 1024 * 1024 * 1024 * 1024}

def size_of(text: str) -> int | None:
    """Bytes in a size such as `512M`, or None when it isn't one."""
    if len(text) < 2 or text[-1] not in UNITS or not text[:-1].isdigit():
        return None
    return int(text[:-1]) * UNITS[text[-1]]

def laid_out(partitions: dict[str, dict[str, typing.Any]]) -> list[dict[str, typing.Any]]:
    """The partitions in the order they go on the disk, each with its `name`."""
    named = [dict(p, name = name) for name, p in sorted(partitions.items())]
    return [p for p in named if p["size"] != "rest"] + [p for p in named if p["size"] == "rest"]

def count_of(devices: dict[str, typing.Any]) -> int:
    return len(devices["paths"]) or devices["count"]

def device_path(p: dict[str, typing.Any], label: str) -> str:
    """Where a partition is once booted: an encrypted one is opened under its label."""
    if p["encrypt"]:
        return "/dev/mapper/" + label
    return "/dev/disk/by-partlabel/" + label

def grow_root(ctx: Context) -> None:
    """A system without disks of its own boots from an image, whose root is no bigger than what's
    on it. Its partition and filesystem grow to fill the disk at boot, which does nothing once
    they have. A partition laid out by hand that repart can't fit, such as one ending off its
    4K grain, keeps its size, and its filesystem still grows to fill it. The definitions stay out
    of /etc/repart.d, where systemd's own repart unit would try them on every boot and fail
    once the root fills its disk."""
    ctx.file("/etc/grow-root/50-root.conf", formats.systemd({
        "Partition": {"Type": "linux-generic"},
    }))
    ctx.unit("grow-root.service", {
        "Unit": {
            "Description": "Grow the root to fill its disk",
            "ConditionVirtualization": "!container",
        },
        "Service": {
            "Type": "oneshot",
            "ExecStart": [
                "-/run/system/systemd/bin/systemd-repart --dry-run=no --definitions=/etc/grow-root",
                "/run/system/systemd/lib/systemd/systemd-growfs /",
            ],
        },
        "Install": {"WantedBy": "multi-user.target"},
    })

def render(ctx: Context) -> None:
    disks = ctx.read("disks")
    filesystems = ctx.read("filesystems")
    if not disks and not filesystems:
        grow_root(ctx)
        return
    partitions = []
    mounts = []
    used = {}
    # Each partition's devices once booted, one per disk that has it, by `<disk>.<partition>`.
    device_paths = {}
    for name, d in disks.items():
        devices = d["devices"]
        count = count_of(devices)
        if count < 1:
            ctx.error(["disks", name, "devices", "count"], "a disk needs at least one device")
        if "at_least" in devices and size_of(devices["at_least"]) == None:
            ctx.error(["disks", name, "devices", "at_least"], "a size is like `500G`")
        parts = laid_out(d["partitions"])
        rest = [p for p in parts if p["size"] == "rest"]
        if len(rest) > 1:
            ctx.error(["disks", name, "partitions"], "only one partition can take the `rest`")
        for p in parts:
            at = ["disks", name, "partitions", p["name"]]
            if p["size"] != "rest" and size_of(p["size"]) == None:
                ctx.error(at + ["size"], "a size is like `1G`, or `rest`")
            if ("format" in p) != ("mount" in p) and p.get("format") != "swap":
                ctx.error(at, "a partition with a filesystem needs a mount, and one with a " +
                              "mount needs its filesystem")
            ref = name + "." + p["name"]
            used[ref] = p
            device_paths[ref] = []
            for n in range(count if p["on"] == "all" else 1):
                label = "%s%d-%s" % (name, n, p["name"])
                device_paths[ref].append(device_path(p, label))
                partitions.append({
                    "label": label,
                    "disk": name,
                    "device": n,
                    "size": p["size"],
                    "format": p.get("format", ""),
                    "encrypt": p["encrypt"],
                })
            if "format" in p:
                mounts.append({
                    "path": p.get("mount", ""),
                    "device": device_paths[ref][0],
                    "format": p["format"],
                    "options": p["options"],
                })
    spanning = []
    for name, fs in filesystems.items():
        at = ["filesystems", name]
        members = []
        for ref in fs["on"]:
            if ref not in used:
                ctx.error(at + ["on"], "there's no partition `%s`; partitions are named as " % ref +
                          "`<disk>.<partition>`")
            if "format" in used[ref]:
                ctx.error(at + ["on"], "`%s` has a filesystem of its own" % ref)
            members.extend(device_paths[ref])
        if fs["raid"] == "raid1" and len(members) < 2:
            ctx.error(at + ["raid"], "raid1 needs at least two partitions")
        if "mount" in fs and fs["subvolumes"]:
            ctx.error(at + ["mount"], "a filesystem with subvolumes mounts them, not itself")
        # Booting with a member missing mounts degraded, after waiting for every member; otherwise
        # the mount names each member, so it waits for all of them.
        degraded = fs["raid"] != "single" and fs["degraded"] == "boot"
        extra = ["degraded"] if degraded else ["device=" + member for member in members[1:]]
        if "mount" in fs:
            mounts.append({
                "path": fs["mount"],
                "device": members[0],
                "format": fs["format"],
                "options": fs["options"] + extra,
            })
        for sub, s in fs["subvolumes"].items():
            if "mount" in s:
                mounts.append({
                    "path": s["mount"],
                    "device": members[0],
                    "format": fs["format"],
                    "options": ["subvol=" + sub] + s["options"] + extra,
                })
        spanning.append({
            "name": name,
            "format": fs["format"],
            "raid": fs["raid"],
            "degraded": "boot" if degraded else "stop",
            "members": members,
            "subvolumes": sorted(fs["subvolumes"].keys()),
        })
    state = ctx.read("state")
    state_path = state.get("path", "")
    if state_path:
        mounted = [m["path"] for m in mounts]
        if state_path not in mounted:
            ctx.error("state.path", "nothing is mounted at `%s` to keep things in" % state_path)
        if "/" in mounted:
            ctx.error("state.path", "with `state.path`, `/` is erased at every boot, so it " +
                      "can't be mounted from a disk")
        mounts.append({
            "path": "/",
            "device": "none",
            "format": "tmpfs",
            "options": ["mode=755", "size=" + state["root_size"]],
        })
    paths = [m["path"] for m in mounts if m["path"]]
    seen = {}
    for path in paths:
        if path in seen:
            ctx.error(["filesystems"], "`%s` is mounted twice" % path)
        seen[path] = True
    if "/" not in paths:
        ctx.error(["disks"], "nothing is mounted at `/`")
    # The tools for what's laid out, to look after it once installed.
    formats_used = [p["format"] for p in partitions] + [fs["format"] for fs in spanning]
    if any([p["encrypt"] for p in partitions]):
        ctx.package("cryptsetup")
    if "btrfs" in formats_used:
        ctx.package("btrfs-progs")
    if "vfat" in formats_used:
        ctx.package("dosfstools")
    # The system an image boots has only the image's root. It still has the tools above, which
    # `wrath install` lays the disks out with, and the checks above, as the configured system.
    if ctx.read("image.live"):
        grow_root(ctx)
        return
    ctx.base("storage", {
        "unlock": ctx.read("unlock"),
        "disks": {
            name: {
                "count": count_of(d["devices"]),
                "kind": d["devices"]["kind"],
                "at_least": d["devices"].get("at_least", ""),
                "paths": d["devices"]["paths"],
            }
            for name, d in disks.items()
        },
        "partitions": partitions,
        "mounts": sorted(mounts, key = lambda m: m["path"]),
        "state": state_path,
        "spanning": spanning,
    })

module(
    name = "disks",
    options = {
        "disks": types.map(disk),
        "filesystems": types.map(filesystem),
        "unlock": option(unlock, default = {}),
        "image.live": option(
            types.bool,
            default = False,
            doc = "Whether this is the system an image boots: from the image's own root, without " +
                  "the disks, filesystems and kept state the configuration describes, which " +
                  "`wrath install` puts the configured system on. `wrath build --image` sets it.",
        ),
        "state.path": option(
            types.path,
            optional = True,
            doc = "A mount to keep things in. With this, `/` is erased at every boot.",
        ),
        "state.root_size": option(
            types.str,
            default = "25%",
            doc = "How much memory `/` can use, as a size or a share of memory.",
        ),
    },
    writes = [],
    render = render,
)
