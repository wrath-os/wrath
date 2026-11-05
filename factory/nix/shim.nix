# Maps the lowered model onto NixOS options. This is fixed code: everything that differs between
# systems comes from `distroInput`, the JSON that `distro` writes, so no Nix is generated.
#
# The model's own files, units, packages and users are added as they are. Settings of the base
# system are defaults, so on a system that extends an existing NixOS configuration, that
# configuration wins where the two overlap. Moving a setting to TOML means deleting it in Nix.
{
  lib,
  pkgs,
  distro,
  distroInput,
  ...
}:

let
  input = lib.importJSON distroInput;
  base = input.base;
  session = base.session or null;
  package = name: lib.getAttrFromPath (lib.splitString "." name) pkgs;
  inherit (lib) mkDefault;
in
{
  system.stateVersion = mkDefault "26.05";

  environment.systemPackages = [ distro ] ++ map package input.packages;

  environment.etc = lib.mapAttrs (_: file: { inherit (file) text mode; }) input.etc // {
    "distro/model.json".text = input.model;
    "distro/model-hash".text = input.model_hash;
  };

  # Files outside /etc are links into the store.
  systemd.tmpfiles.rules = lib.mapAttrsToList (
    path: file: "L+ ${path} - - - - ${pkgs.writeText (baseNameOf path) file.text}"
  ) input.files;

  systemd.units = lib.mapAttrs (_: unit: {
    inherit (unit) text;
    wantedBy = unit.wanted_by;
    requiredBy = unit.required_by;
  }) input.units;

  networking.hostName = mkDefault base.system.hostname;
  time.timeZone = mkDefault base.system.timezone;

  users.mutableUsers = mkDefault false;
  users.users = lib.mapAttrs (
    _: user:
    {
      inherit (user)
        description
        group
        home
        shell
        ;
      extraGroups = user.extra_groups;
      packages = map package user.packages;
      isSystemUser = user.system;
      isNormalUser = !user.system;
      createHome = !user.system;
    }
    // lib.optionalAttrs (user ? uid) { inherit (user) uid; }
  ) base.users;
  # A system user's group has the user's name unless it says otherwise.
  users.groups = lib.mapAttrs (_: _: { }) (
    lib.filterAttrs (name: user: user.system && user.group == name) base.users
  );

  # The firewall module renders its own ruleset.
  networking.firewall.enable = mkDefault false;

  boot.loader.systemd-boot.enable = mkDefault (base.boot.loader == "systemd-boot");
  boot.kernelParams = base.boot.kernel_params;

  networking.useNetworkd = mkDefault true;
  networking.useDHCP = mkDefault base.networking.dhcp;
  # Nothing else asks for the network to be up, but the system is expected to reach it.
  systemd.targets.network-online.wantedBy = [ "multi-user.target" ];

  services.pipewire = lib.mkIf base.audio.enable {
    enable = true;
    alsa.enable = true;
    pulse.enable = true;
  };
  security.rtkit.enable = lib.mkIf base.audio.enable true;
  # PipeWire is socket activated; start it with the user's session so audio is ready.
  systemd.user.services.pipewire.wantedBy = lib.mkIf base.audio.enable [ "default.target" ];

  programs.niri.enable = lib.mkIf (session != null && session.name == "niri") true;
  services.greetd = lib.mkIf (session != null) {
    enable = true;
    settings =
      {
        default_session.command = mkDefault "${pkgs.tuigreet}/bin/tuigreet --cmd niri-session";
      }
      // lib.optionalAttrs (session ? autologin) {
        initial_session = {
          user = session.autologin;
          command = "niri-session";
        };
      };
  };
}
