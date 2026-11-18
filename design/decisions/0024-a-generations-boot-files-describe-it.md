# 0024: A generation's boot files describe it

Status: accepted, 2026-11-11. Supersedes 0022.

## Context

0022 added a `boot.json` per generation, after NixOS's bootspec (RFC 0125). Bootspec exists
because NixOS has several boot loader backends and tools such as its Secure Boot signer, each of
which used to read a generation's files its own way. We have one reader, the activator, and one
boot loader, systemd-boot.

Other systemd-based systems keep no such document. A boot entry in the Boot Loader Specification
is three lines (`linux`, `initrd`, `options`), or one unified kernel image that holds the
kernel, initrd and command line together. The image is its own description.

## Decision

`boot.json` goes. A generation's `kernel` and `initrd` links and its `kernel-params` are what
boots it, one file per line of a boot entry, with `initrd-credentials` naming files on the
machine the initrd takes as credentials (0042; `initrd-secrets` before). The activator reads
these and adds `init=` from the generation's path.

When Secure Boot comes, the generation gains a unified kernel image, and that image is what
boots it.

## Consequences

- One description fewer to keep in step with the files it repeated.
- Generations built with a `boot.json` still boot: their files are all there.
