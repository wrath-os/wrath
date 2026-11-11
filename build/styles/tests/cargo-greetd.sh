# Cargo checks each vendored crate against a hash in this file, and its Cargo.lock has them.
awk -F'"' '
    /^name = / { name = $2 }
    /^version = / { version = $2 }
    /^checksum = / {
        printf "{\"files\":{},\"package\":\"%s\"}\n", $2 > (".cargo-vendor/" name "-" version "/.cargo-checksum.json")
    }
' Cargo.lock
mkdir -p .cargo
cat > .cargo/config.toml <<CONFIG
[source.crates-io]
replace-with = "vendored"
[source.vendored]
directory = ".cargo-vendor"
CONFIG
export CARGO_HOME=/build/cargo RUSTFLAGS="--remap-path-prefix=/build/src=/src"
cargo build --release --offline --locked -j $NPROC
mkdir -p $DEST$PREFIX/bin
cp target/release/greetd target/release/agreety $DEST$PREFIX/bin/
