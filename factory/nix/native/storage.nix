# The disks `distro install` laid out, as the disks module describes them: unlocked in the initrd
# by the names install gave them, the root and the store mounted under /sysroot before switching,
# and everything else mounted once booted. Without disks, this adds nothing.
{
  lib,
  pkgs,
  storage,
}:

let
  inherit (storage) unlock;
  ssh = unlock.ssh.authorized_keys != [ ];
  encrypted = lib.filter (partition: partition.encrypt) storage.partitions;
  # Filesystems across disks that boot with a member missing, and the members that may be.
  degraded = lib.filter (fs: fs.degraded == "boot") storage.spanning;
  mapperName = lib.removePrefix "/dev/mapper/";
  optional = lib.concatMap (fs: map mapperName (lib.tail fs.members)) degraded;

  crypttab = map (
    partition:
    let
      options = [
        "luks"
        "discard"
      ]
      ++ lib.optional (lib.elem partition.label optional) "nofail"
      ++ lib.optional unlock.tpm2 "tpm2-device=auto";
      device = "/dev/disk/by-partlabel/${partition.label}";
    in
    "${partition.label} ${device} none ${lib.concatStringsSep "," options}"
  ) encrypted;

  filesystems = lib.filter (mount: mount.format != "swap") storage.mounts;
  # The initrd mounts what the system runs from: the root, and the store under it.
  early = lib.filter (
    mount: mount.path == "/" || mount.path == "/nix" || lib.hasPrefix "/nix/" mount.path
  ) filesystems;
  line =
    prefix: mount:
    let
      options = if mount.options == [ ] then "defaults" else lib.concatStringsSep "," mount.options;
      where = if mount.path == "/" && prefix != "" then prefix else prefix + mount.path;
    in
    "${mount.device} ${where} ${mount.format} ${options} 0 0";
  swaps = map (mount: "${mount.device} none swap defaults 0 0") (
    lib.filter (mount: mount.format == "swap") storage.mounts
  );

  # Each degraded filesystem waits for its members a while, then mounts with what's there and
  # leaves a note for `distro status`.
  wait =
    fs:
    pkgs.writeShellScript "distro-wait-${fs.name}" ''
      for _ in $(${pkgs.coreutils}/bin/seq 30); do
        missing=""
        for member in ${lib.escapeShellArgs fs.members}; do
          [ -e "$member" ] || missing="$missing $member"
        done
        [ -z "$missing" ] && exit 0
        ${pkgs.coreutils}/bin/sleep 1
      done
      message="distro: ${fs.name} boots degraded, without$missing"
      echo "$message"
      echo "$message" > /dev/console
      ${pkgs.coreutils}/bin/mkdir -p /run/distro
      echo "$missing" > /run/distro/degraded-${fs.name}
    '';
in
lib.optionalAttrs (storage.partitions != [ ]) {
  # Secret files from the machine's disk the initrd needs, added to it on the machine when its
  # boot entries are written, so they never go in the store.
  initrdSecrets = lib.optional ssh "/etc/ssh/initrd_ed25519_key";

  etc = {
    "fstab".text = lib.concatLines (map (line "") filesystems ++ swaps);
  }
  // lib.optionalAttrs (crypttab != [ ]) {
    "crypttab".text = lib.concatLines crypttab;
  };

  initrd = {
    disks = true;
    fstab = map (line "/sysroot") early;
    inherit crypttab;
    # btrfs holds back a filesystem until every member is there, so with one missing it would
    # never mount; the members of a degraded filesystem count as ready anyway.
    udevRules = lib.concatMapStrings (
      fs: lib.concatMapStrings (member: ''
        ENV{DM_NAME}=="${mapperName member}", ENV{SYSTEMD_READY}="1"
      '') fs.members
    ) degraded;
    units = lib.listToAttrs (
      map (
        fs:
        lib.nameValuePair "distro-wait-${fs.name}.service" {
          text = ''
            [Unit]
            Description=Wait for the disks of ${fs.name}
            DefaultDependencies=no
            After=cryptsetup.target
            Before=sysroot.mount

            [Service]
            Type=oneshot
            ExecStart=${wait fs}
          '';
          required_by = [ "sysroot.mount" ];
        }
      ) degraded
    );
    # Unlocking over SSH: the initrd brings up the network and runs sshd with the keys allowed.
    ssh = lib.optionalAttrs ssh {
      inherit (unlock.ssh) port;
      authorizedKeys = unlock.ssh.authorized_keys;
    };
    # systemd loads the TPM's libraries only when asked to, and they load the device driver.
    dlopenFeatures = lib.optional unlock.tpm2 "tpm";
    storePaths = map wait degraded ++ [
      "${pkgs.coreutils}/bin/seq"
      "${pkgs.coreutils}/bin/sleep"
      "${pkgs.coreutils}/bin/mkdir"
    ];
    kernelModules =
      lib.optionals (encrypted != [ ]) [
        "dm_crypt"
        "dm_mod"
        # The ciphers and hashes LUKS containers use.
        "aes"
        "aesni_intel"
        "xts"
        "cbc"
        "sha256"
        "sha512"
        "cryptd"
        "af_alg"
        "algif_skcipher"
      ]
      ++ lib.optionals unlock.tpm2 [
        "tpm_crb"
        "tpm_tis"
      ]
      # Drivers for the disks themselves, on real machines and virtual ones.
      ++ [
        "ahci"
        "nvme"
        "sd_mod"
        "usb_storage"
        "uas"
        "xhci_pci"
        "virtio_pci"
        "virtio_blk"
        "virtio_scsi"
      ]
      # Packet sockets, which DHCP uses, and network drivers, for unlocking over SSH.
      ++ lib.optionals ssh [
        "af_packet"
        "virtio_net"
        "e1000e"
        "igb"
        "igc"
        "ixgbe"
        "r8169"
        "tg3"
        "bnxt_en"
        "mlx5_core"
      ]
      ++ lib.unique (map (mount: mount.format) early)
      # btrfs checksums with crc32c, which it loads by name.
      ++ lib.optionals (lib.any (mount: mount.format == "btrfs") filesystems) [
        "crc32c"
        "crc32c_generic"
      ];
  };
}
