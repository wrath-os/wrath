# The system's message bus, which services such as logind and polkit talk over.

def render(ctx: Context) -> None:
    ctx.service_package("dbus")
    # Services that lose the bus don't all find it again: logind doesn't, and then nobody can log
    # in. A new bus waits for the next boot.
    for unit in ["dbus.service", "dbus.socket"]:
        ctx.unit(unit + ".d/wrath.conf", {"Unit": {"X-RestartIfChanged": "false"}})

module(name = "dbus", options = {}, writes = [], render = render)
