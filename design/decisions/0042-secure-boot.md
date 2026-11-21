# 0042: Secure Boot with keys per machine, signed on the machine

Status: accepted, 2026-11-18.

## Context

A machine boots systemd-boot, one entry per generation, with the kernel and initrd copied to the
ESP and the command line, `init=` naming the generation, in the entry (0024). Disks unlock with
the TPM bound to PCR 7 (0011). Without Secure Boot that binding protects little: PCR 7 has a value
with Secure Boot off too, systemd-boot loads any initrd on the ESP, and anyone at the machine can
edit the command line. The initrd's secrets sit in a plaintext `secrets-N.cpio` on the ESP.

How others do it:

- Talos signs on a build machine: one unified kernel image (UKI: kernel, initrd and command line
  in one signed file) per release, with the UKI and the TPM policy signed by separate keys. The
  machine holds no keys. Its ISO enrolls the keys through systemd-boot in setup mode, and disks
  unlock with PCR 7 plus a signed PCR 11 policy, so kernel updates don't re-enroll. It works
  because every machine boots the same image.
  https://docs.siderolabs.com/talos/v1.13/platform-specific-installations/bare-metal-platforms/secureboot
- lanzaboote and sbctl sign on the machine, since generations are built there. A stub that
  keeps kernels and initrds shared on the ESP has to make sure a separately signed kernel can't
  boot with an unprotected initrd, which signing nothing but the one stub does. Trust has to come
  from exactly one signed object. https://github.com/nix-community/lanzaboote
- Booting through shim, signed by Microsoft's third-party CA, trusts every boot loader signed
  under that CA, older ones included, which downgrade attacks use (BitPixie, 38C3).
- Published since 2025: swapping in a LUKS volume with the same UUIDs gets a PCR 7 binding to
  hand over the real key (oddlama, 2025; IncusOS CVE-2026-32606), fixed by releasing the key only
  in the initrd and checking PCR 15. https://oddlama.org/blog/bypassing-disk-encryption-with-tpm2-unlock/

wrath's generations differ per machine and are put together on the machine, from a cache or its
own builds (0040, 0041). Whatever signs them has to be there.

## Decision

- **One UKI per generation**, made with systemd's own stub, with the generation's command line
  inside it. With Secure Boot, a command line passed at boot is ignored. The ESP is sized at
  install and keeps the last generations' UKIs, older ones losing their entries first, as now.
  No stub of our own.
- **Keys per machine, made ahead** with the host key (`wrath secret machine NAME`), kept in
  `secrets/machines/NAME/`:
  - PK and KEK, which change what the firmware trusts, encrypted to the admins only. They never
    reach the machine.
  - A db key, which signs the machine's UKIs and systemd-boot, and a separate PCR key, which
    signs its TPM policy, encrypted to the machine and the admins. The machine decrypts them as
    secrets, into `/run/wrath/secrets`, never into the store.
- **Signed on the machine.** Switching to a generation signs its UKI and the boot loader with the
  machine's db key, and the UKI carries a PCR 11 policy signed with its PCR key. root on a running
  machine can therefore sign something that unlocks its disks: the tradeoff of signing on the
  machine, which the PIN closes where it matters.
- **Enrollment through systemd-boot**: install puts the machine's certificates in
  `/loader/keys/`, and systemd-boot enrolls them when the firmware is in setup mode, which is the
  one manual step per machine, spelled out by the plan. The firmware trusts our certificate only:
  Microsoft's option ROM CAs are added when the TPM event log shows cards that need them, and
  never its third-party boot loader CA or a vendor's own keys. dbx stays as it is.
- **Unlocking with the TPM**: PCR 7 plus a policy signed with the PCR key for PCR 11, limited to
  the initrd (`enter-initrd`, its own policyref), and PCR 15 checked before the initrd hands over,
  against a swapped LUKS volume. Install makes a recovery key, encrypted to the admins. The
  passphrase stays.
- **A PIN, optional per machine**: `[unlock] pin = true` asks for it along with the TPM, which
  also stands against a stolen machine's key being extracted or sniffed.
- **The initrd's secrets as TPM-sealed credentials** beside the UKI, in place of the plaintext
  `secrets-N.cpio`.
- Configuration:

  ```toml
  [boot]
  secure_boot = true

  [unlock]
  tpm2 = true
  pin = true    # optional
  ```

  Secure Boot is off unless a configuration says otherwise: a machine without it boots the same
  UKIs, unsigned, and installs and upgrades the same way, so machines that leave it off, or whose
  firmware can't do it, aren't a separate path.

## Consequences

- systemd moves to 262, which has the initrd-only policy and the PIN hardening.
- A machine with Secure Boot boots only what it signed, and its disks unlock only in an initrd
  it signed. Every generation it ever signed stays bootable and unlocks until the PCR key is
  rotated, which is how a generation with a bad kernel is revoked.
- A firmware update that changes dbx changes PCR 7: the machine asks for the recovery key or
  passphrase once and enrolls again.
- Losing the PK and KEK leaves the machine's db as it is until its firmware is reset to setup
  mode; losing the db or PCR key means making new ones and enrolling them in setup mode.

## Order

1. systemd 262, with the world rebuilt. Done.
2. UKIs per generation, unsigned, booted through systemd-boot without Secure Boot; the initrd's
   secrets as TPM-sealed credentials. Done: the first is the initrd's SSH host key, for unlocking
   over SSH.
3. Keys made with `wrath secret machine`, signing at switch, enrollment from `/loader/keys/`:
   scenarios in a VM (OVMF in setup mode, swtpm) where an unsigned UKI doesn't boot, a signed one
   unlocks, a changed PCR 7 asks, and a swapped LUKS volume is refused. Keys, signing and
   enrollment done, 2026-11-19 (`native-secure-boot`):
   - `wrath secret machine` makes whichever keys a machine lacks, so a machine installed before
     gets its Secure Boot keys alone. They're kept as two archives: `secure-boot.age`, the db and
     PCR keys and the `.auth` files, encrypted to the recipients and the machine's host key, and
     `secure-boot-owner.age`, the PK and KEK, encrypted to every recipient but the machine.
   - A system with `boot.secure_boot` carries each machine's `secure-boot.age` as a secret for
     that machine only, found by its name. Its switch decrypts it into a directory only root
     reads, under `/run`, for as long as it signs, rather than into `/run/wrath/secrets`.
   - The keys go in `/loader/keys/auto`, which systemd-boot enrolls by itself in a VM with
     `secure-boot-enroll if-safe`, and offers in its menu on other machines.
   - Each image carries `.pcrpkey`, the PCR key's public half, and `.pcrsig`, a policy signed
     for the PCR 11 values of its `enter-initrd` phase only (`native-secure-boot-tpm`). The
     switch keeps the public half in `/var/lib/wrath`, since the stub hands it to the initrd
     only, and `wrath unlock enroll` binds the disks to PCR 7 and to that key's PCR 11 policy,
     once the firmware has Secure Boot on.
   - PCR 15 is changed for `fixate-volume-key=`: measuring every volume's key into PCR 15 would
     keep a second TPM-unlocked disk from unsealing, and PCR 15 alone doesn't stop a swapped
     volume from being handed the key. `fixate-volume-key=` pins the real volume's key hash in
     crypttab. The hash is the machine's own, while the initrd's crypttab is the system's, shared
     by a role. Install works it out for each volume it opens (HMAC-SHA256 with the volume key
     over `cryptsetup:NAME:UUID`, as systemd does) and keeps it in `/var/lib/wrath/volume-keys`;
     `wrath unlock enroll` does too, for machines installed before. Each image the switch signs
     gets the machine's crypttab with the hashes as a small archive after its initrd, which the
     kernel unpacks over the one in the initrd. A volume with the same label and UUID and another
     key is refused, given the passphrase too (`native-secure-boot-tpm`). Without Secure Boot,
     images aren't pinned: anything on the ESP could be replaced anyway.
   - An image changed after it was signed is refused, and the boot loader falls back to an entry
     the machine signed (`native-secure-boot`).
   - Past the initrd, the policy's signature taken out of the image doesn't fit PCR 11 as it is
     then (`native-secure-boot-tpm`). Done, 2026-11-20.
4. The PIN and the recovery key.
   - `unlock.pin = true` seals the TPM key with a PIN too. Install and `wrath unlock enroll` ask
     for it (or take `$NEWPIN`), and the boot asks for it where it asked for nothing. The
     passphrase stays, for when the TPM won't.
   - Install makes a recovery key for every encrypted volume (`systemd-cryptenroll
     --recovery-key`), and `wrath unlock recovery` makes a new one, replacing the old. Where
     it's kept is the one part that varies, so it's an option with a default rather than a
     fixed place:

     ```toml
     [unlock.recovery]
     keep = "esp"     # the default; or "print", or "none"
     command = []     # or a program that takes it instead
     ```

     `esp` encrypts it to `system.secret_recipients` but the machine, so the machine can't read
     it back, puts it on the ESP, where it survives the disks being locked, and prints it once.
     `wrath unlock recovery-key --from DIR` reads it with an admin's key, on any machine. `print`
     shows it once and keeps nothing. `command` hands it to a program on stdin, with the
     machine's name as its argument: a password manager's, a vault's, a module's own. A module
     that ships such a program sets `command`, so a configuration only enables the module.
5. The Framework: firmware updated, its event log checked for option ROMs, setup mode by hand.
   Done on firmware 0.770, 2026-11-21. Its event log loads no option ROMs, so the firmware
   trusts the machine's keys only. What the VM hadn't shown:
   - The kernel needed securityfs for the event log, and efivarfs built in: as a module nothing
     loaded it, so the firmware's variables weren't there and Secure Boot read as off. The
     scenario now checks both.
   - Insyde's setup mode: deleting the PK alone keeps dbx, and "Enforce Secure Boot" has to be
     on, or the firmware leaves Secure Boot off once the keys are in.
   - systemd-boot warns that enrolling can leave a machine unbootable, which the event log
     check is there to rule out.

## Rejected

- Signing only on a builder, as Talos does: a machine putting a generation together from a cache
  couldn't boot it without the builder. It stays a later option for `apply --host`, so a server
  can hold no keys.
- One key for every machine: one machine's root could sign for all of them.
- A stub of our own to share kernels on the ESP: new trusted code to save space we size anyway.
- shim and Microsoft's third-party CA: trusts every other signed boot loader.

## Open

- Rotating the PCR key as a verb, revoking everything signed before.
- Signing off the machine for `apply --host`, and keys on a YubiKey.
- systemd-pcrlock, for policies that can be revoked without a new key.
- Sealing the root's contents to the UKI (dm-verity or composefs), as bootc does.
