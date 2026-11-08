# The system's message bus, which services such as logind and polkit talk over.

def render(ctx):
    ctx.service_package("dbus")

module(name = "dbus", options = {}, writes = [], render = render, base = True)
