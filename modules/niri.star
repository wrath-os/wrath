# The niri session: the compositor, run as a user service; the desktop portals screen sharing
# and file pickers go through, wlroots' for the screen and GTK's for the rest; and GNOME Keyring
# for secrets.

PORTALS = """[preferred]
default=gtk
org.freedesktop.impl.portal.Access=gtk
org.freedesktop.impl.portal.FileChooser=gtk
org.freedesktop.impl.portal.Notification=gtk
org.freedesktop.impl.portal.ScreenCast=wlr
org.freedesktop.impl.portal.Screenshot=wlr
org.freedesktop.impl.portal.Secret=gnome-keyring
"""

# niri reads a user's own config, or else this one. Either way it watches the file and what it
# includes, and applies a change as soon as it's saved.
SYSTEM_CONFIG = "/etc/niri/config.kdl"
USER_CONFIG = "~/.config/niri/config.kdl"
DEFAULTS = "/run/system/share/niri/default-config.kdl"

def config(layers: list[dict[str, typing.Any]]) -> str:
    """niri's own defaults, then each layer of settings. A later setting wins over an earlier
    one, and a bind replaces the one for the same keys."""
    return "".join(['include "%s"\n' % DEFAULTS] + [formats.kdl(layer) for layer in layers])

def render(ctx: Context) -> None:
    desktop = ctx.read("desktop")
    system = ctx.read("programs.niri")["settings"]
    users = {
        user: u["programs"]["niri"]["settings"]
        for user, u in ctx.read("users").items()
    }
    if desktop.get("session") != "niri":
        if system:
            ctx.error("programs.niri.settings", "niri is the desktop only with session = \"niri\"")
        return
    for package in [
        "niri",
        "xdg-desktop-portal",
        "xdg-desktop-portal-gtk",
        "xdg-desktop-portal-wlr",
        "gnome-keyring",
        # The accessibility bus GTK programs ask for, started by D-Bus.
        "at-spi2-core",
    ]:
        ctx.service_package(package)
    # niri runs X programs through it, and it runs Xwayland.
    ctx.package("xwayland-satellite")
    ctx.package("xwayland")
    # The Compose tables dead keys and compose sequences come from, which xkbcommon reads from
    # libX11's share/X11/locale.
    ctx.package("libX11")
    ctx.file("/etc/xdg/xdg-desktop-portal/niri-portals.conf", PORTALS)
    ctx.file(SYSTEM_CONFIG, config([system]))
    for user, own in users.items():
        if own:
            ctx.file(USER_CONFIG, config([system, own]), user = user)
    # The shell starts with the graphical session niri brings up.
    if desktop.get("shell") == "noctalia":
        ctx.service_package("noctalia")
        ctx.enable("noctalia.service", user = True)

SETTINGS = types.record(
    settings = option(
        types.freeform,
        default = {},
        doc = "niri's settings, over its defaults, written as KDL: a table is a section, " +
              "`true` a flag such as `prefer-no-csd`, and a key can carry arguments, such as " +
              "`'output \"eDP-1\"'`. A list of tables or lists repeats a node, such as " +
              "`window-rule` or `spawn-at-startup`.",
    ),
)

module(
    name = "niri",
    options = {"programs.niri": SETTINGS},
    user_options = {"programs.niri": SETTINGS},
    writes = [],
    render = render,
)
