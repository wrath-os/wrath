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

def render(ctx: Context) -> None:
    desktop = ctx.read("desktop")
    if desktop.get("session") != "niri":
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
    # The shell starts with the graphical session niri brings up.
    if desktop.get("shell") == "noctalia":
        ctx.service_package("noctalia")
        ctx.enable("noctalia.service", user = True)

module(name = "niri", options = {}, writes = [], render = render)
