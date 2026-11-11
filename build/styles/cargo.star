# Programs in Rust, built by Cargo with every crate its Cargo.lock names: each crate from the
# registry fetched and checked against the lock's hash, and each repository the lock takes crates
# from at the commit it pins. Nothing is resolved: a project without a lock can't use this style.

REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"

# Crates that link a library from the system, and what building them needs. A lock naming one
# brings its library, so a recipe needn't list it.
NATIVE = {
    "libdbus-sys": ["dbus", "pkgconf"],
    "openssl-sys": ["openssl", "pkgconf"],
}

# What those crates are told, so they use the system's library: openssl-sys builds its own from
# source, which needs Perl, when a project turns on its `vendored` feature.
NATIVE_ENV = {
    "openssl-sys": {"OPENSSL_NO_VENDOR": "1"},
}

# Crates go in a directory of their own, since some projects ship a `vendor` of theirs.
VENDOR = """# Cargo checks each vendored crate against a hash in this file, and its Cargo.lock has them.
awk -F'"' '
    /^name = / { name = $2 }
    /^version = / { version = $2 }
    /^checksum = / {
        printf "{\\"files\\":{},\\"package\\":\\"%s\\"}\\n", $2 > (".cargo-vendor/" name "-" version "/.cargo-checksum.json")
    }
' Cargo.lock
mkdir -p .cargo
cat > .cargo/config.toml <<CONFIG
[source.crates-io]
replace-with = "vendored"
[source.vendored]
directory = ".cargo-vendor"
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
        "into": ".cargo-vendor/%s-%s" % (name, version),
        "sha256": package["checksum"],
    }

def repository(source: str) -> tuple[str, ...]:
    """The repository and commit of a lock's `git+URL?query#COMMIT` source."""
    url, _, commit = source.removeprefix("git+").partition("#")
    if not commit:
        fail("%s names no commit" % source)
    return url.partition("?")[0], commit

def git_checkout(url: str, commit: str) -> dict[str, typing.Any]:
    """The commit, from any host, in `git/` beside the source, under the repository's name. Cargo
    makes any path dependency inside the workspace a member of it, so a crate there would take
    the project's workspace settings in place of its own."""
    return {
        "url": "git+%s#%s" % (url, commit),
        "into": "../git/" + url.removesuffix(".git").rpartition("/")[2],
    }

def native(lock: dict[str, typing.Any]) -> list[str]:
    """The system libraries the lock's crates link, as `NATIVE` names them."""
    deps = []
    for package in lock["package"]:
        for dep in NATIVE.get(package["name"], []):
            if dep not in deps:
                deps.append(dep)
    return deps

def build(recipe: Recipe, options: dict[str, typing.Any]) -> dict[str, typing.Any]:
    lock = decode.toml(recipe.read(options["lock"]))
    # Crates the project itself patches from git in place of crates.io. Cargo doesn't patch a
    # patch, so those are patched for crates.io, and the rest for the repository they come from.
    manifest = decode.toml(recipe.read("Cargo.toml", default = ""))
    replaced = manifest.get("patch", {}).get("crates-io", {})
    sources, patches, fetched = [], {}, {}
    for package in lock["package"]:
        source = package.get("source")
        if source == None:
            continue
        if source == REGISTRY:
            sources.append(registry_crate(package))
        elif source.startswith("git+"):
            url, commit = repository(source)
            checkout = git_checkout(url, commit)
            if url not in fetched:
                fetched[url] = True
                sources.append(checkout)
            name = package["name"]
            table = "crates-io" if "git" in replaced.get(name, {}) else "\"%s\"" % url
            patches.setdefault(table, []).append((name, checkout["into"]))
        else:
            fail("`%s` comes from %s, which isn't fetched" % (package["name"], source))

    prepare = ""
    if options["lock"] != "Cargo.lock":
        prepare = "cp %s Cargo.lock\n" % options["lock"].removeprefix("recipes/")
    found = ""
    if patches:
        found = CRATE_IN
        for table, crates in patches.items():
            found += "echo '[patch.%s]' >> .cargo/config.toml\n" % table
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
    env = {}
    for package in lock["package"]:
        env.update(NATIVE_ENV.get(package["name"], {}))
    env.update(options["env"])
    lines = ["export %s=%s" % item for item in env.items()]
    if options["vendor_only"]:
        # The same sources for Cargo wherever another build tool runs it from, and no network.
        lines += [
            "mkdir -p $CARGO_HOME",
            "sed 's|directory = \".cargo-vendor\"|directory = \"/build/src/.cargo-vendor\"|' " +
            ".cargo/config.toml > $CARGO_HOME/config.toml",
            "printf '[net]\\noffline = true\\n' >> $CARGO_HOME/config.toml",
        ]
        programs = []
    else:
        lines.append(command)
    if programs:
        lines += [
            "mkdir -p $DEST$PREFIX/bin",
            "cp %s $DEST$PREFIX/bin/" % " ".join(["target/release/" + p for p in programs]),
        ]
    return {
        "script": prepare + VENDOR + HOME + found + "".join([line + "\n" for line in lines]) +
                  options["install"],
        "deps": ["rust-bin"] + native(lock),
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
        "vendor_only": option(
            types.bool,
            default = False,
            doc = "Only fetch and vendor the lock's crates, for a project another build tool " +
                  "builds, such as Meson calling Cargo; `install` builds it.",
        ),
        "install": option(
            types.str,
            default = "",
            doc = "Shell lines run after the programs are installed, for files beyond them.",
        ),
    },
    build = build,
)
