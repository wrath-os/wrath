# Projects built with Meson and Ninja, configured with the `-D` options a recipe gives.

def build(recipe: Recipe, options: dict[str, typing.Any]) -> dict[str, typing.Any]:
    setup = "meson setup /build/b %s --prefix=$PREFIX --libdir=lib --buildtype=release" % (
        options["source"]
    )
    lines = ["export %s=%s" % item for item in options["env"].items()] + [
        " ".join([setup] + options["args"]),
        "ninja -C /build/b -j$NPROC",
        "DESTDIR=$DEST ninja -C /build/b install",
    ]
    return {
        "script": options["prepare"] + "".join([line + "\n" for line in lines]) +
                  options["install"],
        "deps": ["meson", "ninja", "pkgconf"],
    }

style(
    name = "meson",
    files = ["meson.build"],
    options = {
        "args": option(
            types.list(types.str),
            doc = "Arguments for `meson setup`, such as `-D docs=disabled`.",
        ),
        "source": option(types.str, default = ".", doc = "The directory with `meson.build`."),
        "env": option(types.map(types.str), doc = "Variables the build has."),
        "prepare": option(types.str, default = "", doc = "Shell lines run before the setup."),
        "install": option(
            types.str,
            default = "",
            doc = "Shell lines run after the install, for what it leaves out.",
        ),
    },
    build = build,
)
