# Fonts for a desktop on a system built without NixOS: fontconfig finds the system's fonts with
# its programs.

FONTS_CONF = """<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">
<fontconfig>
  <dir>/run/current-system/sw/share/fonts</dir>
  <cachedir prefix="xdg">fontconfig</cachedir>
</fontconfig>
"""

def render(ctx):
    if "session" not in ctx.read("desktop"):
        return
    ctx.package("dejavu_fonts")
    ctx.file("/etc/fonts/fonts.conf", FONTS_CONF)

module(name = "fonts", options = {}, writes = [], render = render, base = True)
