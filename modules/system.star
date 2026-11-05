# The machine's identity: hostname and timezone. The backend writes them where the system
# expects them.

def render(ctx):
    system = ctx.read("system")
    ctx.base("system", {"hostname": system["hostname"], "timezone": system["timezone"]})

module(
    name = "system",
    options = {
        "system.hostname": option(types.str, doc = "The machine's hostname."),
        "system.timezone": option(types.str, default = "UTC"),
    },
    writes = [],
    render = render,
)
