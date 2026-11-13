# The base system: boot, networking, audio and the graphical session. The backend provides
# these from the base settings this module works out: the kernel's command line, and what the
# options say.

# The label of the root filesystem wrath's images have, which a system without disks of its own
# boots from.
ROOT_LABEL = "wrath-root"

# Where the booted generation's firmware is, which the kernel reads before /etc is there.
FIRMWARE = "/run/system-booted/firmware"

def kernel_params(ctx: Context, boot: dict[str, typing.Any]) -> list[str]:
    """The kernel's command line. With disks of its own, the initrd mounts the root from them, so
    it isn't named; without, or as the system an image boots, it's the image's root filesystem,
    by its label."""
    params = []
    if ctx.read("image.live") or (not ctx.read("disks") and not ctx.read("filesystems")):
        params = ["root=LABEL=" + ROOT_LABEL, "rw"]
    params += boot["kernel_params"]
    if ctx.read("hardware")["firmware"]:
        params.append("firmware_class.path=" + FIRMWARE)
    return params

def render(ctx: Context) -> None:
    boot = ctx.read("boot")
    ctx.base("boot", {"loader": boot["loader"], "kernel_params": kernel_params(ctx, boot)})
    ctx.base("networking", {
        "dhcp": ctx.read("networking.dhcp"),
        "resolved": ctx.read("networking.resolved"),
    })
    ctx.base("audio", {"enable": ctx.read("audio.enable")})

    desktop = ctx.read("desktop")
    if "session" not in desktop:
        return
    session = {"name": desktop["session"]}
    if "autologin" in desktop:
        user = desktop["autologin"]
        if user not in ctx.read("users"):
            ctx.error("desktop.autologin", "there is no user `%s`" % user)
        session["autologin"] = user
    ctx.base("session", session)

module(
    name = "base",
    options = {
        "boot": types.record(
            loader = option(types.one_of(["systemd-boot"]), default = "systemd-boot"),
            kernel_params = option(types.list(types.str)),
        ),
        "networking.dhcp": option(
            types.bool,
            default = True,
            doc = "Configure every wired interface with DHCP.",
        ),
        "networking.resolved": option(
            types.bool,
            default = True,
            doc = "Run systemd-resolved for DNS, which `/etc/resolv.conf` points at.",
        ),
        "audio.enable": option(types.bool, default = False, doc = "Run PipeWire for users."),
        "desktop": types.record(
            session = option(
                types.one_of(["niri"]),
                optional = True,
                doc = "The graphical session greetd starts.",
            ),
            autologin = option(
                types.str,
                optional = True,
                doc = "Start the session for this user at boot without a login.",
            ),
            shell = option(
                types.one_of(["noctalia"]),
                optional = True,
                doc = "The desktop shell around the compositor: its bar, launcher, " +
                      "notifications and lock screen.",
            ),
        ),
    },
    writes = [],
    render = render,
)
