# The base of a system: the programs every system has, how systemd finds
# them, and consoles to log in on.

SYSTEM = "/run/system"
LOCALES = "system-locales"
CERTIFICATES = "/etc/ssl/certs/ca-certificates.crt"
JOURNAL_DIR_CONF = "/etc/tmpfiles.d/journal.conf"

PACKAGES = [
    "bash",
    "bc",
    "bubblewrap",
    "ca-certificates",
    "coreutils",
    "diffutils",
    "findutils",
    "gawk",
    # getent, ldd, locale and iconv.
    "glibc",
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
    ctx.unit(unit + ".d/wrath.conf", {
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

def os_release(ctx: Context) -> None:
    """/etc/os-release, which says what the system is called: in the boot menu, to programs that
    check, and to people asking."""
    name = ctx.read("system.name")
    id = ctx.read("system.id")
    plain = id.replace("-", "").replace("_", "").replace(".", "")
    if not plain.isalnum() or id.lower() != id:
        ctx.error("system.id", "an id is lowercase letters, digits, `-`, `_` and `.`")
    if '"' in name or "\\" in name or "\n" in name:
        ctx.error("system.name", "a name can't have quotes, backslashes or line breaks in it")
    ctx.file("/etc/os-release", lines([
        'NAME="%s"' % name,
        "ID=" + id,
        'PRETTY_NAME="%s"' % name,
    ]))

def render(ctx: Context) -> None:
    for package in PACKAGES:
        ctx.package(package)
    os_release(ctx)
    # device-mapper's udev rules, which tools such as cryptsetup wait on.
    ctx.service_package("device-mapper")

    # Services find the system's programs, as a login shell does.
    path = "PATH=/run/wrappers/bin:%s/bin" % SYSTEM
    locale_variables = locale_environment(ctx)
    ctx.file("/etc/systemd/system.conf.d/wrath.conf", formats.systemd({
        "Manager": {
            "ManagerEnvironment": path,
            "DefaultEnvironment": [path, "SSL_CERT_FILE=" + CERTIFICATES] +
                                  ["%s=%s" % item for item in locale_variables],
        },
    }))
    ctx.file("/etc/locale.conf", "LANG=%s\n" % ctx.read("system.locale"))
    keymap = ctx.read("system.keymap")
    if keymap:
        ctx.file("/etc/vconsole.conf", "KEYMAP=%s\n" % keymap)
        ctx.restart("systemd-vconsole-setup.service", on = ["/etc/vconsole.conf"])
    # Nothing is enabled behind the configuration's back, on a first boot or any other.
    ctx.file("/etc/systemd/system-preset/00-wrath.preset", "ignore *\n")
    # The kernel loads the modules it needs with this.
    ctx.file("/etc/sysctl.d/50-wrath.conf", "kernel.modprobe = %s/bin/modprobe\n" % SYSTEM)
    ctx.unit("modprobe@.service.d/wrath.conf", {
        "Service": {"ExecSearchPath": SYSTEM + "/bin"},
    })
    # Device nodes for modules not loaded yet, such as /dev/btrfs-control, from the list the
    # booted kernel's modules come with.
    ctx.unit("kmod-static-nodes.service.d/wrath.conf", {
        "Unit": {
            "ConditionFileNotEmpty": [
                "",
                "/run/system-booted/kernel-modules/lib/modules/%v/modules.devname",
            ],
        },
    })

    # Certificate authorities where programs look for them: Debian's and Arch's path, which curl,
    # wget and Go use, the shorter one some others use, and Fedora's, for programs built there.
    # OpenSSL's own directory is in its package, so it's pointed at them too.
    certificates = SYSTEM + "/share/ca-certificates/ca-certificates.crt"
    ctx.file("/etc/tmpfiles.d/ca-certificates.conf", lines([
        "L+ %s - - - - %s" % (path, certificates)
        for path in [CERTIFICATES, "/etc/ssl/cert.pem", "/etc/pki/tls/certs/ca-bundle.crt"]
    ]))

    # The journal on disk, where it lasts across boots and journald keeps each user's messages in
    # a file of their own, which `journalctl --user` reads. Without the directory, journald keeps
    # everything in memory.
    if ctx.read("journal.persistent"):
        ctx.file(JOURNAL_DIR_CONF, "d /var/log/journal 2755 root systemd-journal - -\n")
        ctx.restart("systemd-journal-flush.service", on = [JOURNAL_DIR_CONF])
        # The journal moves to disk before tmpfiles runs at boot, so the directory is made first.
        ctx.unit("journal-directory.service", {
            "Unit": {
                "Description": "The journal's directory on disk",
                "DefaultDependencies": "no",
                "After": "systemd-remount-fs.service",
                "Before": "systemd-journal-flush.service",
                "RequiresMountsFor": "/var/log",
            },
            "Service": {
                "Type": "oneshot",
                "ExecStart": SYSTEM + "/systemd/bin/systemd-tmpfiles --create " + JOURNAL_DIR_CONF,
            },
            "Install": {"WantedBy": "systemd-journal-flush.service"},
        })

    # The login shells there are, which programs such as chsh and some session scripts check.
    ctx.file("/etc/shells", lines(["/bin/sh", SYSTEM + "/bin/sh", SYSTEM + "/bin/bash"]))
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
        ("SSL_CERT_FILE", CERTIFICATES),
    ] + locale_variables
    ctx.file(
        "/etc/profile",
        "".join(["export %s=%s\n" % (name, value) for name, value in environment]) +
        # What a user's `environment` sets, for login shells; systemd reads environment.d itself.
        "if [ -r \"$HOME/.config/wrath/session.sh\" ]; then\n" +
        "  . \"$HOME/.config/wrath/session.sh\"\n" +
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
        "/etc/environment.d/10-wrath.conf",
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
        "journal.persistent": option(
            types.bool,
            default = True,
            doc = "Keep the journal on disk, across boots, rather than in memory.",
        ),
        "system.name": option(
            types.str,
            default = "wrath",
            doc = "What the system calls itself, in the boot menu and `/etc/os-release`.",
        ),
        "system.id": option(
            types.str,
            default = "wrath",
            doc = "The system's id in `/etc/os-release`, lowercase, such as `acme`.",
        ),
    },
    writes = [],
    render = render,
)
