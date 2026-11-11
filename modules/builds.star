# Packages built differently from how their source builds them: with other values for the options
# their build takes, or with patches. The name is what `packages` calls it. Only the package
# itself changes, and what else uses the original keeps the original.

build = types.record(
    package = option(
        types.str,
        optional = True,
        doc = "The package to start from. Without one, the package with the build's name.",
    ),
    options = option(
        types.freeform,
        default = {},
        doc = "Values for the options the package's recipe declares, such as `enableTiny`.",
    ),
    patches = option(
        types.list(types.str),
        doc = "Patch files to apply, relative to the configuration.",
    ),
)

def render(ctx: Context) -> None:
    for name, b in ctx.read("builds").items():
        ctx.build(name, package = b.get("package"), options = b["options"], patches = b["patches"])

module(
    name = "builds",
    options = {"builds": types.map(build)},
    writes = [],
    render = render,
    configurations = ["system", "home", "project"],
)
