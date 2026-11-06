# Where packages come from. Each source has a name and the URL it follows; the lock pins it.
# Packages from a source other than `nixpkgs` say so, as `stable#git`. Setting
# `sources.nixpkgs` changes where the base system and every other package come from.

source = types.record(
    url = option(
        types.str,
        doc = "What the source follows, such as `github:NixOS/nixpkgs/nixos-25.11`.",
    ),
)

def render(ctx):
    for name, s in ctx.read("sources").items():
        if "#" in name:
            ctx.error(["sources", name], "a source's name can't have `#` in it")
        ctx.source(name, s["url"])

module(
    name = "sources",
    options = {
        "sources": types.map(source),
    },
    writes = [],
    render = render,
)
