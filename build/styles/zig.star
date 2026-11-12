# Programs in Zig, built by `zig build` with every package the project's dependency list names,
# each fetched into a directory of its own under its Zig package hash, which `--system` builds
# from with no fetching. Zig reads no lock of its own, so a project that publishes its full list,
# as `build.zig.zon.json` beside `build.zig.zon`, can use this style.

SETUP = """# Zig's caches stay inside the build.
export ZIG_GLOBAL_CACHE_DIR=/build/zig-cache ZIG_LOCAL_CACHE_DIR=/build/zig-local-cache
"""

def package(hash: str, entry: dict[str, typing.Any]) -> dict[str, typing.Any]:
    """Where a dependency comes from and where Zig finds it: a tarball, or a repository at the
    commit it names, as `git+URL#COMMIT`."""
    url = entry["url"]
    if url.startswith("git+") and "#" not in url:
        fail("`%s` names no commit" % url)
    return {"url": url, "into": "../zig-packages/" + hash}

def build(recipe: Recipe, options: dict[str, typing.Any]) -> dict[str, typing.Any]:
    deps = decode.json(recipe.read(options["deps"]))
    sources = [package(hash, entry) for hash, entry in sorted(deps.items())]
    command = " ".join([
        "zig build --system /build/zig-packages -Doptimize=ReleaseFast -Dcpu=baseline",
        "--prefix $PREFIX",
    ] + options["args"])
    lines = ["export %s=%s" % item for item in options["env"].items()] + [
        "DESTDIR=$DEST " + command,
    ]
    return {
        "script": SETUP + "".join([line + "\n" for line in lines]) + options["install"],
        "deps": ["zig-bin", "pkgconf"],
        "sources": sources,
    }

style(
    name = "zig",
    locks = ["build.zig.zon.json"],
    files = ["build.zig"],
    options = {
        "deps": option(
            types.str,
            default = "build.zig.zon.json",
            doc = "The project's list of every package it builds with, by Zig package hash.",
        ),
        "args": option(
            types.list(types.str),
            doc = "Arguments for `zig build`, such as `-fno-sys=simdutf` for a library the " +
                  "system doesn't have, which the project then builds its own copy of.",
        ),
        "env": option(types.map(types.str), doc = "Variables the build has."),
        "install": option(
            types.str,
            default = "",
            doc = "Shell lines run after the install, for what it leaves out.",
        ),
    },
    build = build,
)
