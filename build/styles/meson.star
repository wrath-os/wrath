# Projects built with Meson and Ninja, configured with the `-D` options a recipe gives, which are
# checked against those the project declares before anything builds.

# Meson's own options, which every project has.
BUILTIN = [
    "auto_features", "backend", "bindir", "buildtype", "cmake_prefix_path", "datadir", "debug",
    "default_both_libraries", "default_library", "errorlogs", "force_fallback_for", "includedir",
    "infodir", "install_umask", "layout", "libdir", "libexecdir", "localedir", "localstatedir",
    "mandir", "optimization", "pkg_config_path", "prefer_static", "prefix", "sbindir",
    "sharedstatedir", "stdsplit", "strip", "sysconfdir", "unity", "unity_size", "vsenv",
    "warning_level", "werror", "wrap_mode",
]

def declared(text: str) -> list[str]:
    """The names of the options an options file declares, as `option('name', ...)`."""
    names = []
    for part in text.split("option(")[1:]:
        part = part.lstrip()
        quote = part[:1]
        if quote in ["'", "\""]:
            names.append(part[1:].partition(quote)[0])
    return names

def undeclared(name: str) -> bool:
    """Whether `name` is a compiler's, base or subproject's option, such as `c_args`, `b_lto` or
    `sub:x`, which no options file declares."""
    if ":" in name or "." in name:
        return True
    return "_" in name and name.partition("_")[0] in ["b", "c", "cpp", "objc", "rust"]

def check(recipe: Recipe, source: str, args: list[str]) -> None:
    """Fails on a `-D` option that's neither Meson's own nor one the project declares."""
    at = "" if source == "." else source + "/"
    text = recipe.read(at + "meson.options", default = None)
    if text == None:
        text = recipe.read(at + "meson_options.txt", default = "")
    names = declared(text)
    for arg in args:
        if not arg.startswith("-D"):
            continue
        name = arg.removeprefix("-D").strip().partition("=")[0]
        if name in BUILTIN or name in names or undeclared(name):
            continue
        fail("`%s`: the project has no option `%s`; it has %s" % (
            recipe.id,
            name,
            ", ".join(["`%s`" % n for n in sorted(names)]) or "none of its own",
        ))

def build(recipe: Recipe, options: dict[str, typing.Any]) -> dict[str, typing.Any]:
    check(recipe, options["source"], options["args"])
    setup = "meson setup /build/b %s --prefix=$PREFIX --libdir=lib --buildtype=release" % (
        options["source"]
    )
    lines = ["export %s=%s" % item for item in options["env"].items()] + [
        " ".join([setup] + options["args"]),
        "ninja -C /build/b -j$NPROC",
        "DESTDIR=$DEST ninja -C /build/b install",
    ]
    if options["keep"]:
        kept = " ".join(["-e '^./%s/'" % path for path in options["keep"]])
        lines += [
            "(cd $DEST$PREFIX && find . ! -type d | grep -v %s | xargs -r rm -f)" % kept,
            "find $DEST$PREFIX -depth -type d -empty -delete",
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
        "keep": option(
            types.list(types.str),
            doc = "The directories of the install that stay, such as `share/gir-1.0`, for a " +
                  "package that builds part of another again; empty keeps everything.",
        ),
        "install": option(
            types.str,
            default = "",
            doc = "Shell lines run after the install, for what it leaves out.",
        ),
    },
    build = build,
)
