# Programs in Rust, built by Cargo with every crate its Cargo.lock names: each crate from the
# registry fetched and checked against the lock's hash, and each repository the lock takes crates
# from at the commit it pins. Nothing is resolved: a project without a lock can't use this style.

REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"

VENDOR = """# Cargo checks each vendored crate against a hash in this file, and its Cargo.lock has them.
awk -F'"' '
    /^name = / { name = $2 }
    /^version = / { version = $2 }
    /^checksum = / {
        printf "{\\"files\\":{},\\"package\\":\\"%s\\"}\\n", $2 > ("vendor/" name "-" version "/.cargo-checksum.json")
    }
' Cargo.lock
mkdir -p .cargo
cat > .cargo/config.toml <<CONFIG
[source.crates-io]
replace-with = "vendored"
[source.vendored]
directory = "vendor"
"""

HOME = """CONFIG
export CARGO_HOME=/build/cargo RUSTFLAGS="--remap-path-prefix=/build/src=/src"
"""

def registry_crate(package: dict[str, typing.Any]) -> dict[str, typing.Any]:
    name, version = package["name"], package["version"]
    return {
        "url": "https://static.crates.io/crates/%s/%s-%s.crate" % (name, name, version),
        "into": "vendor/%s-%s" % (name, version),
        "sha256": package["checksum"],
    }

def repository(source: str) -> tuple[str, ...]:
    """The repository and commit of a lock's `git+URL?query#COMMIT` source."""
    url, _, commit = source.removeprefix("git+").partition("#")
    if not commit:
        fail("%s names no commit" % source)
    return url.partition("?")[0], commit

def git_archive(url: str, commit: str) -> dict[str, typing.Any]:
    """The commit as GitHub serves it, under `git/` and the repository's name."""
    if not url.startswith("https://github.com/"):
        fail("crates from %s can't be fetched, only from GitHub" % url)
    path = url.removeprefix("https://github.com/").removesuffix(".git")
    return {
        "url": "https://github.com/%s/archive/%s.tar.gz" % (path, commit),
        "into": "git/" + path.rpartition("/")[2],
    }

def build(recipe: Recipe, options: dict[str, typing.Any]) -> dict[str, typing.Any]:
    lock = decode.toml(recipe.read(options["lock"]))
    sources, patches = [], {}
    for package in lock["package"]:
        source = package.get("source")
        if source == None:
            continue
        if source == REGISTRY:
            sources.append(registry_crate(package))
        elif source.startswith("git+"):
            url, commit = repository(source)
            if url not in patches:
                patches[url] = []
                sources.append(git_archive(url, commit))
            path = options["git"].get(package["name"])
            if path == None:
                fail("`%s` comes from %s: give its path under `git/` in `git`" % (
                    package["name"],
                    url,
                ))
            patches[url].append('%s = { path = "%s" }' % (package["name"], path))
        else:
            fail("`%s` comes from %s, which isn't fetched" % (package["name"], source))

    prepare = ""
    if options["lock"] != "Cargo.lock":
        prepare = "cp %s Cargo.lock\n" % options["lock"].removeprefix("recipes/")
    config = "".join([
        "[patch.\"%s\"]\n%s" % (url, "".join([line + "\n" for line in lines]))
        for url, lines in patches.items()
    ])
    # Crates patched in from git change the lock, so such a build can't be --locked.
    locked = "" if patches else " --locked"
    command = "cargo build --release --offline%s -j $NPROC" % locked
    for package in options["packages"]:
        command += " -p " + package
    if not options["default_features"]:
        command += " --no-default-features"
    if options["features"]:
        command += " --features " + ",".join(options["features"])
    programs = options.get("programs", [recipe.id])
    lines = ["export %s=%s" % item for item in options["env"].items()] + [command]
    if programs:
        lines += [
            "mkdir -p $DEST$PREFIX/bin",
            "cp %s $DEST$PREFIX/bin/" % " ".join(["target/release/" + p for p in programs]),
        ]
    return {
        "script": prepare + VENDOR + config + HOME + "".join([line + "\n" for line in lines]) +
                  options["install"],
        "deps": ["rust-bin"],
        "sources": sources,
    }

style(
    name = "cargo",
    locks = ["Cargo.lock"],
    files = ["Cargo.toml"],
    options = {
        "packages": option(
            types.list(types.str),
            doc = "The workspace's packages to build, as `cargo build -p`. All of them when empty.",
        ),
        "features": option(types.list(types.str), doc = "Cargo features to build with."),
        "default_features": option(
            types.bool,
            default = True,
            doc = "Whether the default features are on.",
        ),
        "programs": option(
            types.list(types.str),
            optional = True,
            doc = "The programs from `target/release` it installs, by default the one named " +
                  "like the recipe. An empty list installs none, for an `install` that does.",
        ),
        "lock": option(
            types.str,
            default = "Cargo.lock",
            doc = "The lock, in the source, or kept beside the recipes as `recipes/locks/...` " +
                  "and listed in `files`, for a project that ships none.",
        ),
        "git": option(
            types.map(types.str),
            doc = "Where each crate the lock takes from git is, under `git/` and its " +
                  "repository's name, such as `git/smithay/smithay-drm-extras`.",
        ),
        "env": option(
            types.map(types.str),
            doc = "Variables the build has, such as `LIBCLANG_PATH` for bindgen.",
        ),
        "install": option(
            types.str,
            default = "",
            doc = "Shell lines run after the programs are installed, for files beyond them.",
        ),
    },
    build = build,
)
