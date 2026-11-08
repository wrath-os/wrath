# Shell that writes `units`, by name, into the unit directory `$out`, with the links that make
# targets want or require them. A name such as `getty@tty1.service.d/autologin.conf` is a
# drop-in.
{ lib, pkgs }:
units:

lib.concatStrings (
  lib.mapAttrsToList (name: unit: ''
    mkdir -p "$(dirname "$out/${name}")"
    ln -sfn ${pkgs.writeText (baseNameOf name) unit.text} "$out/${name}"
    ${lib.concatMapStrings (target: ''
      mkdir -p "$out/${target}.wants"
      ln -sfn "../${name}" "$out/${target}.wants/${name}"
    '') (unit.wanted_by or [ ])}
    ${lib.concatMapStrings (target: ''
      mkdir -p "$out/${target}.requires"
      ln -sfn "../${name}" "$out/${target}.requires/${name}"
    '') (unit.required_by or [ ])}
  '') units
)
