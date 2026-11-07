# The file `link-home.sh` reads for one home: a line `~/path<TAB>target` per file, the target in
# the store, or where the machine fills in a file with secrets in it.
{ lib, pkgs }:
name: home: filled:
let
  # A file of the same name in the store, which systemd wants of a link to a unit.
  target =
    path: file:
    let
      base = baseNameOf path;
    in
    "${
      pkgs.writeTextFile {
        name = lib.strings.sanitizeDerivationName base;
        destination = "/${base}";
        inherit (file) text executable;
      }
    }/${base}";
in
pkgs.writeText "distro-home-${name}" (
  lib.concatStrings (
    lib.mapAttrsToList (path: file: "${path}\t${target path file}\n") (home.files or { })
  )
  + lib.concatStrings (
    lib.mapAttrsToList (path: file: "${path}\t${filled file}\n") (home.templates or { })
  )
)
