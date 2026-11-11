# Graphics drivers for a desktop, packages like any other.

def render(ctx: Context) -> None:
    if "session" not in ctx.read("desktop"):
        return
    ctx.package("mesa")

module(name = "graphics", options = {}, writes = [], render = render)
