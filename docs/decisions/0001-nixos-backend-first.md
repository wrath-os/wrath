# 0001: Build the front end first, on a NixOS backend

Status: accepted, 2026-11-05

## Context

The goal is a declarative, content-addressed system with Nix's model (store, generations,
hermetic builds, fast switching, built-in user environments), configured with TOML and an
embedded language instead of the Nix language.

Building the whole stack bottom-up (own store, own boot and activation, then config) puts the
first day of real use about two years out. Most of that time goes into system integration that
NixOS already does: PAM, setuid wrappers, dbus, polkit, udev, the initrd and the session.
nixpkgs' base packages are also patched for NixOS runtime conventions (`/run/current-system`,
`/run/wrappers`, `/run/opengl-driver`), so booting them without NixOS is a large job by itself.

Stock GNOME and Plasma closures from nixos-unstable are fully available prebuilt on
`cache.nixos.org`, so package builds are not the bottleneck.

## Options

1. Bottom-up: store, then boot without NixOS activation, then config compiler.
2. Front end first: the config compiler emits a backend-neutral resolved model, and the first
   backend lowers it into a NixOS configuration. NixOS layers are then replaced one at a time.
3. Stay on NixOS with a thin TOML wrapper and no plan to replace anything.

## Decision

Option 2.

- The compiler's output is a resolved model: a fully resolved, canonically serialized description
  of one machine. It is the contract between the front end and every backend.
- A fixed shim under `factory/nix/` reads the model as JSON and maps it to NixOS options. No Nix
  code is generated per option.
- The base system (boot, PAM, dbus, polkit, udev, networking, audio, session) uses NixOS's
  high-level options at first. Services, user environments and everything new are rendered by our
  own modules at a low level, so that logic carries over when NixOS is removed.
- After daily use is reached, layers are replaced in order of user value: user environments, the
  activator, the store, then the base system. The same scenarios run before and after each
  replacement.

## Consequences

- The author can use the system daily within months, and every module is tested on real hardware
  from the start.
- `plan` diffs resolved models, so it stays fast even though `apply` pays for NixOS evaluation
  while NixOS is the backend.
- Until the store is replaced, the client runs Nix underneath. Nix still never appears in user
  config or normal CLI output.
- The resolved model has to be designed with care, since every backend depends on it. It is frozen
  by a human at the end of the first phase.
