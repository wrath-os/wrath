# 0022: A generation says what boots it

Status: superseded by 0024, 2026-11-11.

## Context

The activator writes a boot entry per generation by reading the generation's `kernel` and
`initrd` links, its `kernel-params` and its `initrd-secrets`. Each new consumer, such as a Secure
Boot signer or something that builds unified kernel images, would have to know the same layout.
NixOS had the same problem across its boot loader backends and solved it with bootspec (RFC
0125): one JSON document per generation that backends read instead of searching its files.

## Decision

Each generation has a `boot.json`:

```json
{
  "org.distro.boot.v1": {
    "system": "x86_64-linux",
    "label": "distro",
    "kernel": "/store/...-linux-7.3.4/bzImage",
    "initrds": ["/store/.../initrd"],
    "kernel_params": ["quiet", "loglevel=3"]
  }
}
```

Whatever writes boot entries, signs images or builds them reads this alone. From bootspec we take
what worked and what its authors listed as weaknesses:

- The version is in the key. A new version is a new key, and a generation can carry both.
- Optional fields, such as `initrd_secrets` (files on the machine that go in an initrd of their
  own), are left out, never null.
- `initrds` is a list from the start, since a system already has two: its own and the machine's.
- Other tools add keys of their own under reverse-domain names.
- Paths are store paths, so the document's presence means its files are there.

It has no `init` or `toplevel`: a generation can't name itself in its own files, so whoever writes
the entry adds `init=` from the generation's path, as before.

A generation built before this has no `boot.json`; the activator works out the same description
from its files, as bootspec's synthesizer does.

## Consequences

- Secure Boot signing, unified kernel images and boot counting all start from one input.
- The `kernel`, `initrd` and `kernel-params` files stay for now, for older tools and tests, and can
  go once nothing reads them.
