# 0039: Everything wrath owns under /wrath

Status: accepted, 2026-11-15. Changes where 0016 put the store, the generations and the per-boot
directories; the rest of 0016 stands.

## Context

The store was at `/store`, with the generations inside it at `/store/.system`, and wrath's
per-boot directories spread over `/run/wrappers`, `/run/secrets` and `/run/wrath`. Before the
cache gets users and before a machine is installed for real, the layout is the cheapest to
change it will ever be: every key changes with it, so it costs one world rebuild and nothing
else.

What others got wrong, or what confuses people about them:

- `/store` is a generic name. NAS setups, container volumes and backup tools make one. On a
  system of our own that's tolerable; once wrath runs on another distribution, it claims a
  common name on someone else's machine. A store under the tool's own name avoids that.
- State kept in two roots means persisting or moving it takes knowing both.
- Mutable records inside the store stop it from being mounted read-only; ours were in
  `/store/.system`.
- Chains of profile links, from a home through several generations of links to the store, are
  hard to follow.
- A directory like `/etc/static` doesn't say whose it is.

## Decision

```text
/wrath/store/<hash>-<name>    packages and systems, never changed once there
/wrath/system/generations/N   the machine's generations, each a link into the store
/wrath/system/current         the one it boots by default (a link to generations/N)
/wrath/system/{trying,lock,install}   the records switching keeps
/run/system, /run/system-booted       the running and booted system, as 0016 says
/run/wrath/{wrappers,secrets,templates}   per boot
/etc/.wrath                   the one link /etc's files go through, switched in one step
```

- One root: an erased root persists `/wrath` (one subvolume, `subvolumes."@wrath" = { mount =
  "/wrath" }`), and removing wrath from another distribution is removing `/wrath`.
- The store holds store paths and nothing else, so it can be mounted read-only and collection
  never has to skip anything.
- Two hops from a name to a system: `current`, then `generations/N`, then the store.
- `PATH` is `/run/wrath/wrappers/bin:/run/system/bin`.
- `/run/system` stays short and outside `/run/wrath`: units, scripts and `PATH` name it, and it's
  the one path people type.
- Store paths keep the hash first. Tab completion in the store lists hashes, which is awkward,
  but the fixed-length prefix is what finding references and building at a
  path as long as the final one rely on; `wrath why` and `info` are the way to read the store.
- The seed's path covers how it's prepared and where the store is, so a change to either
  prepares it again; the ready marker is keyed by that path.

## Consequences

- Every key and address changes: a world rebuild, and the cache pushed again.
- Configurations that erase their root list `@wrath` mounted at `/wrath` instead of `@store` at
  `/store`.
- A machine already running wrath would need its store and generations moved; none does yet.
