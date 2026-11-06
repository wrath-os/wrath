# Finds a package by the name the model gives it: `git` or `python3Packages.rich` from `pkgs`,
# the system's nixpkgs, and `stable#git` from the flake `sources.stable`, in its `packages` or
# else its `legacyPackages`, as `nix shell` looks them up. Messages starting with `distro: `
# are shown to the user as they are.
lib: pkgs: sources: name:
let
  inherit (pkgs.stdenv.hostPlatform) system;
  parts = lib.splitString "#" name;
  attrIn =
    source: set: attr:
    let
      path = lib.splitString "." attr;
    in
    if lib.hasAttrByPath path set then
      lib.getAttrFromPath path set
    else
      throw "distro: there's no package `${attr}` in ${source}";
in
if builtins.length parts == 1 then
  attrIn "nixpkgs" pkgs name
else
  let
    source = builtins.head parts;
    attr = builtins.elemAt parts 1;
    unknown = "distro: package `${name}` comes from source `${source}`, which isn't in the input";
    flake = sources.${source} or (throw unknown);
  in
  flake.packages.${system}.${attr} or (attrIn source (flake.legacyPackages.${system} or { }) attr)
