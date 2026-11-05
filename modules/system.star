# The machine's identity: hostname and timezone.

def render(ctx):
    system = ctx.read("system")
    ctx.file("/etc/hostname", system["hostname"] + "\n")
    ctx.file("/etc/timezone", system["timezone"] + "\n")

module(
    name = "system",
    options = {
        "system.hostname": option(types.str, doc = "The machine's hostname."),
        "system.timezone": option(types.str, default = "UTC"),
    },
    writes = [],
    render = render,
)
