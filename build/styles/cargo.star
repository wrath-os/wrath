# Programs in Rust, built by Cargo with every crate its Cargo.lock names, each fetched from the
# registry and checked against the lock's hash. Nothing is resolved: a project without a lock
# can't use this style.

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
CONFIG
export CARGO_HOME=/build/cargo RUSTFLAGS="--remap-path-prefix=/build/src=/src"
"""

def crates(lock: dict[str, typing.Any]) -> list[dict[str, typing.Any]]:
    """A source for each crate the lock takes from the registry, by its hash."""
    sources = []
    for package in lock["package"]:
        source = package.get("source")
        if source == None:
            continue
        if source != REGISTRY:
            fail("`%s` comes from %s; only crates from the registry are fetched" % (
                package["name"],
                source,
            ))
        sources.append({
            "url": "https://static.crates.io/crates/%s/%s-%s.crate" % (
                package["name"],
                package["name"],
                package["version"],
            ),
            "into": "vendor/%s-%s" % (package["name"], package["version"]),
            "sha256": package["checksum"],
        })
    return sources

def build(recipe: Recipe, options: dict[str, typing.Any]) -> dict[str, typing.Any]:
    lock = decode.toml(recipe.read("Cargo.lock"))
    command = "cargo build --release --offline --locked -j $NPROC"
    for package in options["packages"]:
        command += " -p " + package
    if not options["default_features"]:
        command += " --no-default-features"
    if options["features"]:
        command += " --features " + ",".join(options["features"])
    programs = options["programs"] or [recipe.id]
    lines = ["export %s=%s" % item for item in options["env"].items()] + [
        command,
        "mkdir -p $DEST$PREFIX/bin",
        "cp %s $DEST$PREFIX/bin/" % " ".join(["target/release/" + p for p in programs]),
    ]
    return {
        "script": VENDOR + "".join([line + "\n" for line in lines]) + options["install"],
        "deps": ["rust-bin"],
        "sources": crates(lock),
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
            doc = "The programs from `target/release` it installs. The one named like the " +
                  "recipe when empty.",
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
