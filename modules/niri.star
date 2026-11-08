# The niri session on a system built without NixOS: the compositor, run as a user service, the
# desktop portals screen sharing and file pickers go through, and GNOME Keyring for secrets.

PORTALS = """[preferred]
default=gnome;gtk
org.freedesktop.impl.portal.Access=gtk
org.freedesktop.impl.portal.FileChooser=gtk
org.freedesktop.impl.portal.Notification=gtk
org.freedesktop.impl.portal.Secret=gnome-keyring
"""

def render(ctx):
    if ctx.read("desktop").get("session") != "niri":
        return
    for package in [
        "niri",
        "xdg-desktop-portal",
        "xdg-desktop-portal-gnome",
        "xdg-desktop-portal-gtk",
        "gnome-keyring",
    ]:
        ctx.service_package(package)
    # niri runs X programs through it.
    ctx.package("xwayland-satellite")
    ctx.file("/etc/xdg/xdg-desktop-portal/niri-portals.conf", PORTALS)

module(name = "niri", options = {}, writes = [], render = render, base = True)
