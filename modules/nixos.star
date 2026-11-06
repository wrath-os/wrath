# Builds on NixOS, so a machine can move to distro one piece at a time: what the TOML doesn't
# cover yet stays in Nix, as an existing configuration to extend or as extra modules. Either
# ties the system to the NixOS backend until it's removed.

def render(ctx):
    extend = ctx.read("nixos.extend")
    modules = ctx.read("nixos.modules")
    replaces = ctx.read("nixos.replaces")
    setting = {}
    if extend != None:
        flake, _, host = extend.partition("#")
        if not flake or not host:
            fail("nixos.extend: expected `<flake>#<host>`, such as `/etc/nixos#laptop`, got `%s`" % extend)
        setting = {"flake": flake, "host": host}
    if modules:
        setting["modules"] = modules
    if replaces:
        setting["replaces"] = replaces
    if setting:
        ctx.backend("nixos", setting)

module(
    name = "nixos",
    options = {
        "nixos.extend": option(
            types.str,
            optional = True,
            doc = "A NixOS configuration, as `<flake>#<host>`, that the system is built on. " +
                  "Settings in it win where they overlap with the TOML.",
        ),
        "nixos.modules": option(
            types.list(types.path),
            default = [],
            doc = "NixOS modules added to the system, such as ones not yet replaced by TOML.",
        ),
        "nixos.replaces": option(
            types.set(types.str),
            default = [],
            doc = "Units, such as `ollama.service`, that the TOML now provides in place of the " +
                  "ones NixOS defines. Any other unit both define is an error.",
        ),
    },
    writes = [],
    render = render,
)
