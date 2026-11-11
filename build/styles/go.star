# Programs in Go, built from the modules their go.sum names, each fetched from the module proxy
# into a proxy of files the build points GOPROXY at. Go checks each against the go.sum itself.

PROXY = "https://proxy.golang.org"

SETUP = """# Modules come from the proxy of files the sources were put in, and from nowhere else.
export GOPROXY=file://$PWD/goproxy GOSUMDB=off GOTOOLCHAIN=local GOFLAGS=-mod=mod CGO_ENABLED=0
export GOCACHE=/build/go-cache GOMODCACHE=/build/go-modules
"""

def escape(path: str) -> str:
    """A module path as the proxy spells it, each capital letter as `!` and the small one."""
    escaped = ""
    for i in range(len(path)):
        c = path[i]
        escaped += "!" + c.lower() if c.isupper() else c
    return escaped

def modules(go_sum: str) -> list[dict[str, typing.Any]]:
    """The proxy's files the go.sum names: each module's go.mod, and the zip of each it vouches
    for, once each."""
    files = []
    seen = {}
    for line in go_sum.splitlines():
        fields = line.split()
        if len(fields) < 3 or not fields[1].startswith("v") or not fields[2].startswith("h1:"):
            continue
        version, kind = fields[1], "zip"
        if version.endswith("/go.mod"):
            version, kind = version.removesuffix("/go.mod"), "mod"
        path = "%s/@v/%s.%s" % (escape(fields[0]), version, kind)
        if path not in seen:
            seen[path] = True
            files.append({"url": PROXY + "/" + path, "into": "goproxy/" + path, "file": True})
    return files

def build(recipe: Recipe, options: dict[str, typing.Any]) -> dict[str, typing.Any]:
    flags = " -ldflags \"%s\"" % options["ldflags"] if options["ldflags"] else ""
    packages = " ".join(options["packages"])
    lines = ["export %s=%s" % item for item in options["env"].items()] + [
        "go build -trimpath%s -o $DEST$PREFIX/bin/ %s" % (flags, packages),
    ]
    return {
        "script": SETUP + "".join([line + "\n" for line in lines]) + options["install"],
        "deps": ["go"],
        "sources": modules(recipe.read("go.sum")),
    }

style(
    name = "go",
    locks = ["go.sum"],
    files = ["go.mod"],
    options = {
        "packages": option(
            types.list(types.str),
            default = ["."],
            doc = "The packages to build, such as `./cmd/age`, each a program in `bin`.",
        ),
        "ldflags": option(
            types.str,
            default = "",
            doc = "Flags for the linker, such as `-X main.Version=v1.2.1`.",
        ),
        "env": option(types.map(types.str), doc = "Variables the build has."),
        "install": option(
            types.str,
            default = "",
            doc = "Shell lines run after the programs are built, for files beyond them.",
        ),
    },
    build = build,
)
