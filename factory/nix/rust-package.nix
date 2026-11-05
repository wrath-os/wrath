# A binary from the workspace, built from only the sources it needs.
{
  lib,
  rustPlatform,
  pname,
  # Crates under `crates/` whose sources the binary needs.
  crates,
  mainProgram,
}:

let
  root = ../..;
  # Every member manifest is needed for the workspace to load, but only the listed crates'
  # sources, so changes elsewhere don't rebuild the binary or the images that contain it.
  manifests = lib.fileset.fileFilter (file: file.name == "Cargo.toml") (root + "/crates");
in
rustPlatform.buildRustPackage {
  inherit pname;
  version = "0.0.0";

  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions (
      [
        (root + "/Cargo.toml")
        (root + "/Cargo.lock")
        manifests
      ]
      ++ map (crate: root + "/crates/${crate}") crates
    );
  };

  # Cargo needs a target in every workspace member, so give the members we left out empty ones.
  postPatch = ''
    for dir in crates/*/; do
      if [ ! -d "$dir/src" ]; then
        mkdir "$dir/src"
        echo 'fn main() {}' > "$dir/src/main.rs"
        touch "$dir/src/lib.rs"
      fi
    done
  '';

  cargoLock.lockFile = root + "/Cargo.lock";
  cargoBuildFlags = [
    "--package"
    pname
  ];
  # Tests run in the workspace's own test suite.
  doCheck = false;

  meta = { inherit mainProgram; };
}
