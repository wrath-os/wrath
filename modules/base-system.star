# The base of a system: the programs every system has, how systemd finds
# them, and consoles to log in on.

SYSTEM = "/run/system"
LOCALES = "system-locales"

PACKAGES = [
    "bash",
    "bubblewrap",
    "coreutils",
    "findutils",
    "gawk",
    "grep",
    "gzip",
    "hostname",
    "sed",
    "tar",
    "xz",
    "iproute2",
    "iputils",
    "kmod",
    "less",
    "openssh",
    "procps",
    "shadow",
    "util-linux",
]

def getty(ctx: Context, unit: str, args: str) -> None:
    ctx.unit(unit + ".d/distro.conf", {
        "Service": {
            "ExecStart": [
                "",
                "-agetty --login-program %s/bin/login %s - ${TERM}" % (SYSTEM, args),
            ],
        },
    })

def locale_environment(ctx: Context) -> list[tuple[str, ...]]:
    """The variables that select the machine's language, with its locale built when it's needed."""
    locale = ctx.read("system.locale")
    environment = [("LANG", locale)]
    if locale != "C.UTF-8":
        charset = locale.partition(".")[2] or "ISO-8859-1"
        ctx.build(
            LOCALES,
            package = "locales",
            options = {"locales": ["%s/%s" % (locale, charset)]},
        )
        ctx.package(LOCALES)
        environment.append(("LOCPATH", SYSTEM + "/lib/locale"))
    return environment

def render(ctx: Context) -> None:
    for package in PACKAGES:
        ctx.package(package)
    # device-mapper's udev rules, which tools such as cryptsetup wait on.
    ctx.service_package("device-mapper")

    # Services find the system's programs, as a login shell does.
    path = "PATH=/run/wrappers/bin:%s/bin" % SYSTEM
    locale_variables = locale_environment(ctx)
    ctx.file("/etc/systemd/system.conf.d/distro.conf", formats.systemd({
        "Manager": {
            "ManagerEnvironment": path,
            "DefaultEnvironment": [path] + ["%s=%s" % item for item in locale_variables],
        },
    }))
    ctx.file("/etc/locale.conf", "LANG=%s\n" % ctx.read("system.locale"))
    keymap = ctx.read("system.keymap")
    if keymap:
        ctx.file("/etc/vconsole.conf", "KEYMAP=%s\n" % keymap)
        ctx.restart("systemd-vconsole-setup.service", on = ["/etc/vconsole.conf"])
    # Nothing is enabled behind the configuration's back, on a first boot or any other.
    ctx.file("/etc/systemd/system-preset/00-distro.preset", "ignore *\n")
    # The kernel loads the modules it needs with this.
    ctx.file("/etc/sysctl.d/50-distro.conf", "kernel.modprobe = %s/bin/modprobe\n" % SYSTEM)
    ctx.unit("modprobe@.service.d/distro.conf", {
        "Service": {"ExecSearchPath": SYSTEM + "/bin"},
    })
    # Device nodes for modules not loaded yet, such as /dev/btrfs-control, from the list the
    # booted kernel's modules come with.
    ctx.unit("kmod-static-nodes.service.d/distro.conf", {
        "Unit": {
            "ConditionFileNotEmpty": [
                "",
                "/run/system-booted/kernel-modules/lib/modules/%v/modules.devname",
            ],
        },
    })

    ctx.file("/etc/hosts", "127.0.0.1 localhost\n::1 localhost\n")
    ctx.file("/etc/nsswitch.conf", lines([
        "passwd: files systemd",
        "group: files systemd",
        "shadow: files",
        "hosts: files myhostname dns",
    ]))
    # Login shells, and the service managers of users' sessions, find the system's programs and
    # what they ship, such as D-Bus services and desktop files.
    environment = [
        ("PATH", "/run/wrappers/bin:%s/users/${USER}/bin:%s/bin" % (SYSTEM, SYSTEM)),
        ("XDG_DATA_DIRS", SYSTEM + "/share"),
        ("XDG_CONFIG_DIRS", "/etc/xdg"),
    ] + locale_variables
    ctx.file(
        "/etc/profile",
        "".join(["export %s=%s\n" % (name, value) for name, value in environment]) +
        # What a user's `environment` sets, for login shells; systemd reads environment.d itself.
        "if [ -r \"$HOME/.config/distro/session.sh\" ]; then\n" +
        "  . \"$HOME/.config/distro/session.sh\"\n" +
        "fi\n",
        next_login = True,
    )
    ctx.file(
        "/etc/pam/environment",
        "".join([
            "%s DEFAULT=%s\n" % (name, value.replace("${USER}", "@{PAM_USER}"))
            for name, value in environment
        ]),
        next_login = True,
    )
    ctx.file(
        "/etc/environment.d/10-distro.conf",
        "".join(["%s=%s\n" % (name, value) for name, value in environment]),
        next_login = True,
    )

    getty(ctx, "getty@.service", "--noreset --noclear")
    getty(ctx, "serial-getty@.service", "--noreset --noclear --keep-baud 115200,57600,38400,9600")

module(
    name = "base-system",
    options = {
        "system.locale": option(
            types.str,
            default = "C.UTF-8",
            doc = "The machine's language and formats, such as `en_US.UTF-8`.",
        ),
        "system.keymap": option(
            types.str,
            optional = True,
            doc = "The keyboard layout on the console, such as `sv-latin1`.",
        ),
    },
    writes = [],
    render = render,
)
