# The base of a system built without NixOS: the programs every system has, how systemd finds
# them, and consoles to log in on.

SW = "/run/current-system/sw"

PACKAGES = [
    "bashInteractive",
    "coreutils",
    "findutils",
    "gawk",
    "gnugrep",
    "gnused",
    "iproute2",
    "iputils",
    "kmod",
    "less",
    "procps",
    "shadow",
    "util-linux",
]

def getty(ctx, unit, args):
    ctx.unit(unit + ".d/distro.conf", {
        "Service": {
            "ExecStart": [
                "",
                "-agetty --login-program %s/bin/login %s - ${TERM}" % (SW, args),
            ],
        },
    })

def render(ctx):
    for package in PACKAGES:
        ctx.package(package)

    # Services find the system's programs, as a login shell does.
    path = "PATH=/run/wrappers/bin:%s/bin" % SW
    ctx.file("/etc/systemd/system.conf.d/distro.conf", formats.systemd({
        "Manager": {"ManagerEnvironment": path, "DefaultEnvironment": path},
    }))
    # Nothing is enabled behind the configuration's back, on a first boot or any other.
    ctx.file("/etc/systemd/system-preset/00-distro.preset", "ignore *\n")
    # The kernel loads the modules it needs with this.
    ctx.file("/etc/sysctl.d/50-distro.conf", "kernel.modprobe = %s/bin/modprobe\n" % SW)
    ctx.unit("modprobe@.service.d/distro.conf", {
        "Service": {"ExecSearchPath": SW + "/bin"},
    })

    ctx.file("/etc/nsswitch.conf", "\n".join([
        "passwd: files systemd",
        "group: files systemd",
        "shadow: files",
        "hosts: files myhostname dns",
    ]) + "\n")
    ctx.file("/etc/profile", "export PATH=/run/wrappers/bin:%s/bin\n" % SW, next_login = True)

    getty(ctx, "getty@.service", "--noreset --noclear")
    getty(ctx, "serial-getty@.service", "--noreset --noclear --keep-baud 115200,57600,38400,9600")

module(
    name = "base-system",
    options = {},
    writes = [],
    render = render,
    base = True,
)
