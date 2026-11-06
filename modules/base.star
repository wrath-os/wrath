# The base system: boot, networking, audio and the graphical session. The backend provides
# these, so this module only checks the options and passes them on as base settings.

def render(ctx):
    boot = ctx.read("boot")
    ctx.base("boot", {"loader": boot["loader"], "kernel_params": boot["kernel_params"]})
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
            kernel_params = option(types.list(types.str), default = []),
        ),
        "networking.dhcp": option(
            types.bool,
            default = True,
            doc = "Configure every wired interface with DHCP.",
        ),
        "networking.resolved": option(
            types.bool,
            default = False,
            doc = "Run systemd-resolved for DNS.",
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
        ),
    },
    writes = [],
    render = render,
)
