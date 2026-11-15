# What next: from the reviews and the research

Four reviews (a NixOS user, a platform engineer, a build-systems engineer, a new user with only
the docs) and three research passes (the builder, configuration, fleets) fed this. Each section
says what's wrong, what others do, the options, a recommendation, and what's open. The quick
fixes are done; this is what needs deciding.

## Short answers first

- **Typo'd groups.** NixOS silently drops a name in `extraGroups` that no `users.groups` entry
  declares; only a missing primary group is an assertion (users-groups.nix,
  update-users-groups.pl). grapes is worse today: any name becomes a new group with a gid.
  Recommendation: a group must be declared, by a module, by systemd's own sysusers files (audio,
  video, input, kvm, render, tty, disk), or as `groups.NAME = {}` in the configuration; anything
  else is an error at the user's line with a did-you-mean.
- **Time zones.** NixOS checks only that there's no space (`addCheck str nospace` in
  locale.nix) and links blindly. grapes already checks the shape. A real check needs tzdata's list
  of zones at plan time, which the provides index below gives for free; until then, nothing cheap
  and correct.
- **"rendered by modules X and Y".** It's an error, not information: two writers of one file. It
  should point at the user's TOML when one writer is the configuration, and say how to take the
  file over (see Escape hatches).
- **Search and `rg` to `ripgrep`.** Needs a metadata index (search) and a provides index (which
  package ships `bin/rg`); see Finding packages.
- **zsh and Nushell.** Done: `shell = "zsh"` (or `fish`, `nushell`) by name, listed in
  `/etc/shells`, and `programs.zsh` and `programs.nushell` for users' settings.

## 1. A binary cache

Without one, every machine builds the world (about two hours now); three of four reviewers called
it the blocker. 0018 designs a per-file manifest cache keyed by the unresolved key.

What the research changes: Nix just dropped "deep realisations", the unresolved-key-to-address
mapping 0018 describes, because an unresolved key hides which dependency outputs went in, so two
honest builders disagree with no way to tell why (Nix 2.35 "build traces"). Nix now keys traces
on resolved inputs only.

Recommendation:

- The cache signs **build key -> output addresses** (the build key names dependencies by their
  content addresses). A client walks the graph from the seed: compute a build key, look it up,
  take the addresses, go on; one batched lookup per level.
- The unresolved key -> build key index stays, unsigned, as a hint to skip work.
- Traces are append-only: several signed traces for one key are kept, never overwritten (Nix's S3
  bug #13037 is the overwrite). Two traces that disagree are a reproducibility report for free.
- Files go up openly; the manifest and address verify them. Only traces need a signature, from a
  key the machine lists.
- Keep 0018's per-file manifests with self-references zeroed: that removes the store-path churn
  that defeats Attic's chunk dedupe. BLAKE3 for file digests leaves room for verified streaming.

Open: who runs the first cache (a static bucket is enough for traces and files), and whether
machines trust one key or K-of-N.

## 2. Building in parallel

Builds run one package at a time, and `NPROC` is fixed at 32. A world rebuild that takes two
hours is mostly waiting.

Others: Nix multiplies `max-jobs` by `cores` (oversubscribes); nixpkgs dropped load averages as
wrong; Gentoo's `--load-average` lags; Bazel admits actions against declared resources. GNU make
4.4, Ninja 1.13, cargo and rustc all speak a FIFO jobserver now.

Recommendation: wrath owns one jobserver FIFO with as many tokens as the machine has cores, and
mounts it into every build (`MAKEFLAGS=--jobserver-auth=fifo:...`). A package starts when it gets
a token; ready packages go by longest remaining path through the graph. cgroups per build: CPU
weight, a memory limit, and no new builds while memory pressure (PSI) is high. Go gets
`GOMAXPROCS` from the tokens it holds. `NPROC` stays out of keys.

## 3. What a dependency gives a build

The builder hard-codes `PYTHONPATH` from `site`, `ACLOCAL_PATH`, run paths. Each new ecosystem is
Rust code and a world rebuild.

Others: Guix declares `search-paths` on the package that reads the variable (python declares
`PYTHONPATH` over `lib/python*/site-packages`), expanded over every input that has the
directories, at build time and in profiles. Nix setup hooks (shell from dependencies) are what not
to do; even the manual says they obscure inputs. Bazel providers are more than this needs.

Recommendation: Guix's design, as data on the consuming recipe:

```toml
[[package.search_paths]]
variable = "PYTHONPATH"
files = ["site"]
```

Python, automake, gobject-introspection and pkgconf declare theirs; the builder expands them over
the build's view. They're part of the recipe, so they're in its key. Generations can use the same
specs at run time (0020's indexes), so `GI_TYPELIB_PATH` and friends work on the machine too.

## 4. Planning without sources

Styles read a package's source (Cargo.lock, meson.options) to write its script, so computing
keys needs every source tarball, even for packages a cache would supply. It's Nix's
import-from-derivation; nixpkgs bans it because evaluation becomes unbounded.

Bazel's answer is closest to ours: module extensions read sources in a fetch phase and record the
result in `MODULE.bazel.lock`.

Recommendation: record what a style read (or the digest of the record it produced) in the
recipe's lock entry (0034's per-recipe lock). Planning reads the lock, never a tarball; the build
re-reads and fails if the facts changed; `wrath update` refreshes them. Keep the local record
cache as an accelerator.

## 5. When the builder itself changes

`BUILD_RULES` is bumped by hand; a missed bump shipped a broken Firefox, and every bump rebuilds
the world. Hashing the builder's source into every key (as graph.rs does) would rebuild the world
on a comment change.

Others: Guix puts its build-side code into each derivation; Nix keeps fixups in stdenv, a hashed
input; Bazel salts.

Recommendation: post-processing becomes its own step. The build makes a raw output, keyed without
post-processing; a fix-up step keyed by (raw output address, hash of the fix-up code) makes the
final output. Changing run-path or splitting rules reruns only fix-ups, seconds each, and content
addresses cut off everything whose output didn't change. This answers my earlier objection: a
comment change reruns fix-ups, not compiles. Keep the golden test as a tripwire.

## 6. Replacing a package for everything that uses it

Today `[builds.NAME]` makes a differently built package under a new name that only that one uses;
patching openssl for everything means forking grapes. 0025 rules out an override layer.

Others: nixpkgs overlays (fixpoints, infinite recursion, no idea what rebuilds); Guix
`package-input-rewriting` and `--with-input` (clean, rebuilds everything); Guix grafts (security
fixes without rebuilding, by rewriting references, ABI-compatible only); Bazel's
`single_version_override`, honoured only in the root module.

Recommendation: one flag on the concept that exists, honoured only in the starting file:

```toml
[builds.openssl-fips]
from = "openssl"
options = { fips = true }
replaces = "openssl"
```

Every dependent's key changes through content addressing, and the plan says "replaces openssl:
214 packages rebuild" with the list. No fixpoint, nothing implicit. Grafts later, marked as such
in the plan, only as a security stopgap until the rebuild lands.

## 7. Escape hatches in modules

A module that lacks a knob means writing a platform. NixOS users lean on `lib.mkForce` on a file
and per-key `serviceConfig` merging; RFC 42's freeform `settings` mostly worked, with the caveat
that settings aren't checked until the program reads them.

Recommendation, all with the existing `force` from 0007, no priorities and no disable lists:

- `files."/etc/ssh/sshd_config" = { text = "...", force = "reason" }` takes the file over from the
  module; the plan names the module it displaced and shows when the displaced content changes.
- Keys merge into module-rendered units like a drop-in: the user's `[units."sshd.service"]` keys
  join the module's; disagreeing keys are an error, `force` replaces one key.
- Service modules grow `settings` (freeform, rendered in the program's format) where the program
  has a structured config, as program settings (0027) already do.

## 8. Finding packages

No `search`, no "which package has `rg`". Everyone splits this in two: search from metadata, and
"provides" from file lists of built outputs, shipped as a separate index (apt-file's Contents,
dnf's filelists, nix-index-database). command-not-found broke for flakes because its index was
tied to channels.

Recommendation: `wrath search` reads recipes (0034's index), no evaluation. A provides index is
built from build outputs' manifests and published with each grapes revision, pinned in the lock
beside grapes, so it matches what you'd get. It answers `rg` -> `ripgrep`, command-not-found, and
checks against what packages really ship: time zones, shells, sysusers groups.

## 9. Fleets

Today: push over SSH from a workstation, try/keep, boot counting. The platform reviewer's
blockers were the cache, a pull path, fleet status, real drift detection, and secrets per role.

What worked elsewhere: keep the rollout decision apart from the reboot decision (Fedora CoreOS:
a rollout graph, then a reboot strategy); a tiny reboot lock (FleetLock, two HTTP calls, several
servers exist); canaries as a channel that gets releases first; health checked locally, after
boot (greenboot's required checks gating `boot-complete.target`); CI builds once and machines
fetch by key (Cachix Deploy, bootc).

Recommendation, keeping one model:

- Intent stays in the repo: `machines.toml` gives a machine its role and channel; a
  `channels.toml` maps channels to commits. Promotion is a commit. `plan --channel` works like
  `--role`.
- CI builds each (role, channel) once, pushes to the cache, and publishes a signed record.
  Machines are pushed to (`apply --host`, unchanged) or pull the record with an agent and apply it
  the same way.
- The plan's tier decides coordination: live and restart changes apply when they arrive;
  reboot-tier changes take a FleetLock slot or wait for a window. No other system has this signal.
- Roles declare health checks that gate `boot-complete.target`, so "boots but broken" rolls back.
- `status --json` is the one report, locally, over SSH and from the agent; `status --verify`
  re-hashes store paths and diffs rendered files in `/etc` against the generation, pointing at the
  TOML that owns each.
- Secrets are encrypted to the roles and machines whose model uses them, which the evaluator
  already knows (agenix-rekey's idea, done properly); role keys for interchangeable machines.
- `install --set k=v` and a run-once first-boot source (Ignition's rules: fail closed, never
  re-read) both write `machine.toml`, checked against the role's `[machine.values]`.
- A/B images stay a platform for later, where every plan is tier reboot.

## 10. Naming

Two words mislead rather than just being unfamiliar:

- **platform** means CPU and OS to everyone from Nix, Rust and LLVM. The key could say what it
  is: `modules = { source = "acme" }`.
- **tier** has no common equivalent; tools describe the effect. Plans could say "takes effect:
  now / on restart / after a soft reboot / after a reboot", with the noun only in reference docs.

Keep **generation** (Nix, Guix and home-manager users know it) and **home**. Say "base system",
never "base" alone. Keep **grapes** as the repository's name, and say "package set" in output.

## 11. The recipe format, before it grows

`probes` (150 recipes) and `fixup` do nothing; `deps` sits beside `extra_deps`; `core`, `toolset`
and `scope` overlap; one option is camelCase. 0034 already plans one file per recipe and a
per-recipe lock. Cleaning up at 385 recipes is cheap; at thousands it isn't.

Done with 0037, 2026-11-15, with parallel builds and fix-ups as their own step.

## 12. The second round of reviews, 2026-11-15

The same four looked again after 0037. All four were happier: planning reads no source, a change
to the builder no longer rebuilds the world, search paths are data, the format debt is gone. What
they still want, most asked first:

- **A binary cache** (all four). Unchanged, and now the thing between this and anyone else's
  machine. The keys it needs are in place.
- **Scheduling.** A fixed number of builds with a fixed `NPROC` each; no jobserver, no memory or
  pressure gating, no longest path first (section 2's recommendation, not built).
- **`replaces` and the other escape hatches** (section 6 and 7). Patching openssl for everything
  still means forking grapes.
- **Variants are copy-paste.** gtk4-gir repeats gtk4's recipe to flip one option and builds GTK
  twice; a build of a styled recipe also styles at plan time, fetching its source.
- **Plans write locks in a checkout** unless given `--locked`. Said per recipe; whether the
  default should flip is open.
- **Smaller:** a typo'd group still becomes a new group; `tier` and `platform` keep their
  names; grapes has no CI to run `dev lint --locked`.

Fixed from the review the same night: a build's key now has what it runs with (the wrapper's flags,
the script around the recipe's, the sandbox's setup), which `BUILD_RULES` covered by hand; boot
packages' keys have the settle step's digest; the newcomer's gaps in the recipes guide and the
first apply after installing; an unknown package a module adds names the module.

Fixed later that night: `strip` runs at the end of each build, in its sandbox, with the build's own
binutils, and only on ELF files, so it's in the key and fix-ups use no host tool (a world
rebuild, 386 packages in 50 minutes, none failed); `--locked` is one flag for every command, and
refuses any write to the configuration's lock or a recipe's; a package's search path
declarations are in its own unresolved key, so dependents see them however the dependency got
into their view; syncthing has a recipe; four recipes propagate what their headers include.

A third, short round on those: all four happier. The NixOS user calls strip fixed properly but
won't move without a cache and overlays; the fleet engineer finds `--locked` sound; the Bazel
engineer finds the search path keying sound and now wants scheduling most; the newcomer likes
one `--locked` and `clean` reclaiming images. What they found, fixed the same night: with locks
only read, a pin in another recipe's lock was accepted for this one, so `dev lint --locked`
missed a lock lacking it; the error blamed a pinned grapes even in a checkout; `-o` linked an
image that `clean` could then remove, so it copies an image now. Left for the next world
rebuild: `STRIP` doesn't reset what it read between files, harmless since `strip` refuses a file
it can't open anyway.

## Order

Each step leaves something usable, and the format changes land in one world rebuild:

1. **Builder foundations:** parallel builds, then fix-ups as their own step. Every later step
   rebuilds less and faster.
2. **One recipe-format change:** per-recipe files and lock (0034), search paths, style facts in
   the lock, and the schema cleanup, together with one rebuild.
3. **The cache:** build traces, manifests, signing; then the provides index published beside it,
   which brings `search`, command-not-found and the real checks (zones, groups, shells).
4. **Configuration:** `replaces`, files taken over with `force`, unit keys merging, declared
   groups.
5. **Fleets:** channels in the repo, `status --json` and `--verify`, secrets from use,
   `install --set`; then the pull agent and FleetLock.
6. **Naming,** whenever, but before anyone outside uses it.

## Questions

1. The cache: one trusted key, or K-of-N from the start? Who hosts the first one?
2. `replaces` on `builds`: comfortable revisiting 0025 this way?
3. Taking a module's file over with `force`: enough, or do service modules also need
   `settings`?
4. Fleets: is a git-held `channels.toml` the right home for "which version runs where", or
   should that live in the cache's signed records only?
5. Rename `platform` and `tier` now?
6. Does the order match what you want next: builder speed first, or the cache first?
