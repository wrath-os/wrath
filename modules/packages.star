# Packages for everyone on the machine, or for a project's shell. A user's own packages go under
# `packages` in `users/<name>.toml`.

def render(ctx: Context) -> None:
    for package in ctx.read("packages"):
        ctx.package(package)

module(
    name = "packages",
    options = {
        "packages": option(types.set(types.str)),
    },
    writes = [],
    render = render,
    configurations = ["system", "project"],
)
