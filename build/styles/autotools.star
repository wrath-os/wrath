# Projects with a `configure` script and a Makefile, as GNU projects ship them.

def build(recipe: Recipe, options: dict[str, typing.Any]) -> dict[str, typing.Any]:
    lines = ["export %s=%s" % item for item in options["env"].items()] + [
        " ".join(["%s/configure --prefix=$PREFIX" % options["source"]] + options["args"]),
        "make -j$NPROC",
        "make install DESTDIR=$DEST",
    ]
    return {
        "script": options["prepare"] + "".join([line + "\n" for line in lines]) +
                  options["install"],
    }

style(
    name = "autotools",
    files = ["configure"],
    options = {
        "args": option(
            types.list(types.str),
            doc = "Arguments for `configure`, such as `--disable-nls`.",
        ),
        "source": option(types.str, default = ".", doc = "The directory with `configure`."),
        "env": option(types.map(types.str), doc = "Variables the build has."),
        "prepare": option(types.str, default = "", doc = "Shell lines run before configuring."),
        "install": option(
            types.str,
            default = "",
            doc = "Shell lines run after the install, for what it leaves out.",
        ),
    },
    build = build,
)
