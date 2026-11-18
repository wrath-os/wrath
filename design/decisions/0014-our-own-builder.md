# 0014: Our own builder and store, leaving Nix and nixpkgs

Status: accepted, 2026-11-10.

## Context

0012 left NixOS and kept nixpkgs and Nix: packages from nixpkgs, built and stored by Nix, behind
the backend. It said replacing them needed its own decision.

Since then a separate experiment, usrview, has built a hermetic, content-addressed builder and
store from scratch, and booted distro configurations with it:

- 88 recipes, from a toolchain to systemd, PAM, shadow, sudo, nginx and the kernel, all built
  from a seed packed from our own toolchain. Nothing from nixpkgs is an input.
- It lowers distro's model into generations made of small actions, so a one-line change reruns
  one action and a switch takes about 100 ms.
- A system from it boots under UEFI with no `/usr`, switches live, and, in a VM set up like the
  Framework Desktop, boots from USB storage to a login prompt where a user logs in and uses `sudo`.
- Each apply keeps who ran it, the configuration, the lock and an explanation, apart from the
  objects a generation is made of, and a diff between applies says where each changed value was
  set.

distro meanwhile carries two systems: its own model and modules, and a Nix flake that turns the
model into a system. Keeping two systems in step is now the part that most limits the measures
in 0012: an evaluation of the second sits in every apply, its store paths and closures shape
what `plan` can say, and package builds can't be traced back to the configuration the way
distro's own files can.

## Decision

distro gets one builder and one store, its own. usrview moves into this repository:

- The store, the sandbox, the seed and the action graph become crates here. Recipes live in the
  repository next to modules.
- The model lowers straight into actions. The backend crate's Nix code, `factory/nix` and the
  `nixpkgs` default source are deleted.
- Modules keep asking for packages by name. Names are ours from now on; the nixpkgs names they use
  today are renamed as recipes arrive.
- The VM harness boots images built by the new store. Test images and the guest agent are built
  the same way.
- The repository's own development environment stays on devenv, which uses nixpkgs. That is a
  tool for working on distro and doesn't reach a system distro builds.

## Order

1. Move the crates and recipes in, building and testing as they do in usrview, with no change to
   distro's commands.
2. `distro build` and `plan` use the new lowering. The UEFI boot, live switch and Framework login
   tests run as scenarios.
3. `apply`, `rollback`, `history` and `why` use the store's applies.
4. The VM harness and its images move to the new store. Scenarios move one at a time.
5. `install` lays out disks and copies a closure from the new store.
6. `factory/nix`, the Nix code in the backend and the `nixpkgs` source are deleted, and the docs
   say so.

The Framework Desktop stick needs only the model, which is already free of Nix, so it keeps working
throughout.

## Consequences

- Until recipes exist, modules that need missing packages can't build: the desktop (pipewire,
  wireplumber, mesa, greetd), fprintd, polkit, openssh, syncthing, tailscale and others. Recipes
  are the main work after the move, and Go and Rust programs need their language builds first.
- Homes, installs and the scenarios built on Nix images stop working at step 6 unless they've
  moved. The acceptance scenarios that boot Nix images need review before they change.
- Package updates come from our recipes. There is no binary cache to lean on, so a cache for the
  store is part of the work.
- Sources in configurations become files to include. A package source like nixpkgs has no
  meaning any more.

## Alternatives

- Keep nixpkgs as the package source and build its derivations without Nix. That keeps the
  coverage, but nixpkgs' build logic is Nix code, so either Nix stays or we reimplement its
  evaluator.
- Wait until the recipes cover every module. Safe, but keeps two systems in step for months and
  holds distro's commands on the slow path the whole time.
- Keep usrview separate and have distro call it. The two share the model, the apply record and
  the lowering already, so a boundary there would be a second interface to keep stable.
