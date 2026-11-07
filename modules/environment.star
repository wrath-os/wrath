# Environment variables a project's shell has, from `environment` in `distro.toml`. A home's are
# under `environment` in `users/<name>.toml`.

def render(ctx):
    for name, v in ctx.read("environment").items():
        if "\n" in v:
            ctx.error(["environment", name], "a value can't span lines")

module(
    name = "environment",
    options = {
        "environment": option(
            types.map(types.str),
            default = {},
            doc = "Variables the project's shell has. Values may refer to others as `$NAME`.",
        ),
    },
    writes = [],
    render = render,
    configurations = ["project"],
)
