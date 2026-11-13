# Running programs built for other distributions: a loader at /lib64 that finds their libraries
# through a cache of its own, which programs wrath builds never read (decision 0015).

USUAL = ["glibc", "gcc", "zlib", "openssl", "curl"]

DESKTOP = [
    "wayland",
    "libxkbcommon",
    "mesa",
    "fontconfig",
    "freetype",
    "libX11",
    "libxcb",
    "alsa-lib",
]

def render(ctx: Context) -> None:
    foreign = ctx.read("programs.foreign")
    if not foreign["enable"]:
        return
    libraries = list(USUAL)
    if ctx.read("desktop").get("session"):
        libraries += DESKTOP
    libraries += [name for name in sorted(foreign["libraries"]) if name not in libraries]
    ctx.base("foreign", {"libraries": libraries})

module(
    name = "foreign",
    options = {
        "programs.foreign": types.record(
            enable = option(
                types.bool,
                default = False,
                doc = "Run dynamically linked programs built for other distributions.",
            ),
            libraries = option(
                types.set(types.str),
                doc = "Packages whose libraries they find, beside glibc's, the C++ runtime, " +
                      "zlib, OpenSSL, curl and, with a desktop, its graphics and sound.",
            ),
        ),
    },
    writes = [],
    render = render,
)
