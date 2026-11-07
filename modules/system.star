# The machine's identity: hostname and timezone. The backend writes them where the system
# expects them. Without a hostname, the machine uses its name, filled in on the machine when it
# switches, so every machine with a role gets the same build.

def render(ctx):
    system = ctx.read("system")
    hostname = system.get("hostname") or machine_value("machine.name")
    if type(hostname) == "runtime_value":
        ctx.file("/etc/hostname", hostname + "\n", 420)
        hostname = ""
    ctx.base("system", {"hostname": hostname, "timezone": system["timezone"]})

module(
    name = "system",
    options = {
        "system.hostname": option(
            types.str,
            optional = True,
            runtime = True,
            doc = "The machine's hostname. Without one, the machine's name.",
        ),
        "system.timezone": option(types.str, default = "UTC"),
    },
    writes = [],
    render = render,
)
