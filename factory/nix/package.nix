# Finds a package by the name the model gives it: `git` or `python3Packages.rich` from `pkgs`,
# the system's nixpkgs, and `stable#git` from the flake `sources.stable`, in its `packages` or
# else its `legacyPackages`, as `nix shell` looks them up.
lib: pkgs: sources: name:
let
  inherit (pkgs.stdenv.hostPlatform) system;
  parts = lib.splitString "#" name;
  attrIn = set: attr: lib.getAttrFromPath (lib.splitString "." attr) set;
in
if builtins.length parts == 1 then
  attrIn pkgs name
else
  let
    source = builtins.head parts;
    attr = builtins.elemAt parts 1;
    flake =
      sources.${source}
        or (throw "package `${name}` comes from source `${source}`, which isn't in the input");
  in
  flake.packages.${system}.${attr} or (attrIn flake.legacyPackages.${system} attr)
