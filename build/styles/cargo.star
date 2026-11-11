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

CRATE_IN = """# A crate the lock takes from git, found by its name among the manifests in its repository.
crate_in() {
    for manifest in $(find "$1" -name Cargo.toml | sort); do
        name=$(awk -F'"' '/^\\[package\\]/ { p = 1; next } /^\\[/ { p = 0 } p && /^name *=/ { print $2; exit }' "$manifest")
        if [ "$name" = "$2" ]; then dirname "$manifest"; return; fi
    done
    echo "$1 has no crate $2" >&2
    return 1
}
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

def git_checkout(url: str, commit: str) -> dict[str, typing.Any]:
    """The commit, from any host, under `git/` and the repository's name."""
    return {
        "url": "git+%s#%s" % (url, commit),
        "into": "git/" + url.removesuffix(".git").rpartition("/")[2],
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
            checkout = git_checkout(url, commit)
            if url not in patches:
                patches[url] = []
                sources.append(checkout)
            patches[url].append((package["name"], checkout["into"]))
        else:
            fail("`%s` comes from %s, which isn't fetched" % (package["name"], source))

    prepare = ""
    if options["lock"] != "Cargo.lock":
        prepare = "cp %s Cargo.lock\n" % options["lock"].removeprefix("recipes/")
    found = ""
    if patches:
        found = CRATE_IN
        for url, crates in patches.items():
            found += "echo '[patch.\"%s\"]' >> .cargo/config.toml\n" % url
            for name, into in crates:
                found += "path=$(crate_in %s %s)\n" % (into, name)
                found += "echo \"%s = { path = \\\"$path\\\" }\" >> .cargo/config.toml\n" % name
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
        "script": prepare + VENDOR + HOME + found + "".join([line + "\n" for line in lines]) +
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
