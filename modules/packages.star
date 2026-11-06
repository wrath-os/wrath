# Packages for everyone on the machine. A user's own packages go under `packages` in
# `users/<name>.toml`.

def render(ctx):
    for package in ctx.read("packages"):
        ctx.package(package)

module(
    name = "packages",
    options = {
        "packages": option(types.set(types.str), default = []),
    },
    writes = [],
    render = render,
)
