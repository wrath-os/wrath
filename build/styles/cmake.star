# Projects built with CMake and Ninja, configured with the `-D` options a recipe gives.

def build(recipe: Recipe, options: dict[str, typing.Any]) -> dict[str, typing.Any]:
    configure = (
        "cmake -S %s -B /build/b -G Ninja -DCMAKE_INSTALL_PREFIX=$PREFIX " +
        "-DCMAKE_INSTALL_LIBDIR=lib -DCMAKE_BUILD_TYPE=Release"
    ) % options["source"]
    lines = ["export %s=%s" % item for item in options["env"].items()] + [
        " ".join([configure] + options["args"]),
        "ninja -C /build/b -j$NPROC",
        "DESTDIR=$DEST ninja -C /build/b install",
    ]
    return {
        "script": options["prepare"] + "".join([line + "\n" for line in lines]) +
                  options["install"],
        "deps": ["cmake", "ninja", "pkgconf"],
    }

style(
    name = "cmake",
    files = ["CMakeLists.txt"],
    options = {
        "args": option(
            types.list(types.str),
            doc = "Arguments for `cmake`, such as `-DBUILD_TESTING=OFF`.",
        ),
        "source": option(types.str, default = ".", doc = "The directory with `CMakeLists.txt`."),
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
