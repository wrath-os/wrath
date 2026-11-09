# Graphics drivers for a desktop. Programs from nixpkgs look for
# drivers under /run/opengl-driver, which points at the system's programs and libraries, so
# drivers are packages like any other.

def render(ctx):
    if "session" not in ctx.read("desktop"):
        return
    ctx.package("mesa")
    ctx.file(
        "/etc/tmpfiles.d/graphics.conf",
        "L+ /run/opengl-driver - - - - /run/current-system/sw\n",
    )

module(name = "graphics", options = {}, writes = [], render = render)
