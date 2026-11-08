# /etc/passwd, /etc/group and /etc/shadow for the model's users. A user without a uid gets the
# next free one in name order, from 1000 for people and down from 999 for system users. Groups a
# user names are created when nothing else does; systemd-sysusers adds the system's own at boot.
{ lib, pkgs }:
users:

let
  names = builtins.attrNames users;
  given = lib.filter (uid: uid != null) (map (name: users.${name}.uid or null) names);
  allocate =
    start: step: wanted:
    let
      next =
        taken: uid: if lib.elem uid taken then next taken (uid + step) else uid;
      go =
        acc: name:
        let
          uid = next (given ++ lib.attrValues acc) start;
        in
        acc // { ${name} = uid; };
    in
    lib.foldl' go { } wanted;
  without = system: lib.filter (n: !(users.${n} ? uid) && users.${n}.system == system) names;
  allocated = allocate 1000 1 (without false) // allocate 999 (-1) (without true);
  uidOf = name: users.${name}.uid or allocated.${name};

  fixedGroups = {
    root = 0;
    wheel = 1;
    users = 100;
    nogroup = 65534;
  };
  # A system user's group has its name unless it says otherwise, with the user's id.
  ownGroups = lib.listToAttrs (
    map (name: lib.nameValuePair name (uidOf name)) (
      lib.filter (name: users.${name}.system && users.${name}.group == name) names
    )
  );
  named = lib.unique (
    lib.concatMap (name: [ users.${name}.group ] ++ users.${name}.extra_groups) names
  );
  extraNames = lib.filter (group: !(fixedGroups ? ${group} || ownGroups ? ${group})) named;
  gids =
    fixedGroups
    // ownGroups
    // lib.listToAttrs (lib.imap0 (i: group: lib.nameValuePair group (400 + i)) extraNames);
  members =
    group: lib.filter (name: lib.elem group users.${name}.extra_groups) names;

  shellOf =
    name:
    if users.${name} ? shell then
      users.${name}.shell
    else if users.${name}.system then
      "/run/current-system/sw/bin/nologin"
    else
      "/run/current-system/sw/bin/bash";

  passwd = lib.concatLines (
    [
      "root:x:0:0:System administrator:/root:/run/current-system/sw/bin/bash"
      "nobody:x:65534:65534:Unprivileged account:/var/empty:/run/current-system/sw/bin/nologin"
    ]
    ++ map (
      name:
      let
        user = users.${name};
      in
      "${name}:x:${toString (uidOf name)}:${toString gids.${user.group}}:${user.description}:${user.home}:${shellOf name}"
    ) names
  );
  group = lib.concatLines (
    lib.mapAttrsToList (
      name: gid: "${name}:x:${toString gid}:${lib.concatStringsSep "," (members name)}"
    ) gids
  );
  shadow = lib.concatLines (map (name: "${name}:!:1::::::") ([ "root" ] ++ names));
in
{
  passwd = pkgs.writeText "passwd" passwd;
  group = pkgs.writeText "group" group;
  shadow = pkgs.writeText "shadow" shadow;
}
