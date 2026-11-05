{ lib, rustPlatform }:

let
  root = ../..;
  # Only the agent's sources plus every member manifest, so changes to other crates don't
  # rebuild the agent or the test images that contain it.
  manifests = lib.fileset.fileFilter (file: file.name == "Cargo.toml") (root + "/crates");
in
rustPlatform.buildRustPackage {
  pname = "distro-agent";
  version = "0.0.0";

  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      (root + "/Cargo.toml")
      (root + "/Cargo.lock")
      manifests
      (root + "/crates/agent")
    ];
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
    "distro-agent"
  ];
  # The agent's tests run in the workspace's own test suite.
  doCheck = false;

  meta.mainProgram = "distro-agent";
}
