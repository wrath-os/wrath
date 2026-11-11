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
    if ctx.read("desktop").get("session") != "niri":
        return
    for package in [
        "niri",
        "xdg-desktop-portal",
        "xdg-desktop-portal-gtk",
        "xdg-desktop-portal-wlr",
        "gnome-keyring",
    ]:
        ctx.service_package(package)
    # niri runs X programs through it, and it runs Xwayland.
    ctx.package("xwayland-satellite")
    ctx.package("xwayland")
    ctx.file("/etc/xdg/xdg-desktop-portal/niri-portals.conf", PORTALS)

module(name = "niri", options = {}, writes = [], render = render)
