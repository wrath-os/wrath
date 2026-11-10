# Finds a package by the name the model gives it: `git` or `python3Packages.rich` from `pkgs`,
# the system's nixpkgs, and `stable#git` from the flake `sources.stable`, in its `packages` or
# else its `legacyPackages`, as `nix shell` looks them up. A name in `builds` is the package it
# starts from, built with the options and patches given there. Messages starting with `distro: `
# are shown to the user as they are.
lib: pkgs: sources: builds:
let
  inherit (pkgs.stdenv.hostPlatform) system;
  # distro's names for packages, where nixpkgs calls them something else.
  nixpkgsNames = {
    bash = "bashInteractive";
    grep = "gnugrep";
    sed = "gnused";
    dejavu-fonts = "dejavu_fonts";
    linux = "linuxPackages";
    linux-latest = "linuxPackages_latest";
    device-mapper = "lvm2";
    locales = "glibcLocales";
  };
  attrIn =
    source: set: attr:
    let
      path = lib.splitString "." attr;
    in
    if lib.hasAttrByPath path set then
      lib.getAttrFromPath path set
    else
      throw "distro: there's no package `${attr}` in ${source}";
  find =
    name:
    let
      parts = lib.splitString "#" name;
    in
    if builtins.length parts == 1 then
      attrIn "nixpkgs" pkgs (nixpkgsNames.${name} or name)
    else
      let
        source = builtins.head parts;
        attr = builtins.elemAt parts 1;
        unknown =
          "distro: package `${name}` comes from source `${source}`, which isn't in the input";
        flake = sources.${source} or (throw unknown);
      in
      flake.packages.${system}.${attr}
        or (attrIn source (flake.legacyPackages.${system} or { }) attr);

  withOptions =
    name: base: options:
    let
      known = lib.functionArgs base.override;
      unknown = lib.filter (option: !(known ? ${option})) (builtins.attrNames options);
    in
    if options == { } then
      base
    else if !(base ? override) then
      throw "distro: build `${name}`: `${base.pname or base.name}` has no options to set"
    else if unknown != [ ] then
      throw "distro: build `${name}`: its package has no option `${builtins.head unknown}`"
    else
      base.override options;

  withPatches =
    base: patches:
    if patches == [ ] then
      base
    else
      base.overrideAttrs (old: {
        patches =
          (old.patches or [ ])
          ++ lib.imap0 (i: text: pkgs.writeText "distro-${toString i}.patch" text) patches;
      });

  # nixpkgs builds every locale unless told otherwise; distro's locales recipe builds those given.
  optionsFor =
    build: lib.optionalAttrs (build.package == "locales") { allLocales = false; } // build.options;
  built =
    name: build: withPatches (withOptions name (find build.package) (optionsFor build)) build.patches;
in
name: if builds ? ${name} then built name builds.${name} else find name
