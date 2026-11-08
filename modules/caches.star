# Where built packages can be fetched from, besides the default cache, each with the key that
# signs what it serves.

SETTINGS = "/etc/nix/settings.conf"

def render(ctx):
    caches = ctx.read("caches")
    if not caches:
        return
    ctx.file(SETTINGS, "".join([
        "extra-substituters = %s\n" % " ".join(sorted(caches.keys())),
        "extra-trusted-public-keys = %s\n" % " ".join(sorted([c["key"] for c in caches.values()])),
    ]))
    ctx.restart("nix-daemon.service", on = [SETTINGS])

module(
    name = "caches",
    options = {
        "caches": types.map(types.record(
            key = option(types.str, doc = "The public key that signs what this cache serves."),
        )),
    },
    writes = [],
    render = render,
    base = True,
)
