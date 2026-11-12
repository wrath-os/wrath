# The store

How distro builds and stores systems, as the experiments that led to it support it. They ran in a
separate repository, usrview, whose `results/` hold the evidence; the step numbers below refer to
them. This is a working design: any part of it can change if something better turns up. Each
section says how firm it is.

## The idea

Packages build against an ordinary `/usr` inside a sandbox, install to a path of their own, and run
with no `/usr` on the machine. A system is a generation: a directory naming the packages and
holding the generated `/etc`. Switching systems is one atomic rename.

Unmodified upstream build systems worked: 60 of 60 packages ran from their final location, and
systemd reached `running` from a disk with no `/usr`, with no patches to its source.

## Decisions

### The store is plain files

`/store/<hash>-<name>` is an ordinary directory on an ordinary filesystem, read-only to everything
but the tool that adds to it. Adding a package means unpacking one directory, with nothing to mount
and no switch. That's what makes running a package ad hoc possible, and live switches need old and
new paths visible together.

A package holds only files, directories and links, without setuid or setgid: the store is copied
as root, so a setuid `sudo` in it would stay setuid root on every machine for as long as the path
lives. A system's wrappers are where privileges come from.

Firm. erofs, composefs and plain ext4 booted at the same speed here (step 11), though that's a weak
test on a warm host. Plain files also work on any platform.

### Sealed images are an output, not the store

A generation can be written out as an erofs or composefs image for appliance-style systems, with
no change to how packages are built. Nothing about the store depends on it. Untested beyond
booting one (step 11).

### Packages are built with real prefixes and a compiler wrapper

`--prefix` is the store path. The wrapper sets the interpreter and library search path at link
time. The build sees a composed `/usr` holding exactly the declared dependencies.

Firm. A view that's too narrow silently loses features, and one that's too wide silently changes
output (step 6). The arm that patched binaries afterwards did worst. Store paths may have names,
which the experiment's nameless paths didn't test.

### Dependencies are exact hashes

Every program names exactly what it runs with. Several versions coexist with no global state, and
a binary's closure can be read from its references.

Decided for now. The alternative, library slots, works and allows swapping a library live, but it
makes the exact library set a property of the generation (step 12).

### Packages split into outputs

`lib`, `out`, `doc` and `dev`, split as a package is built and hashed in that order. A reference
only points backwards in that list, so each output has its own hash. Dependents see every output
except `doc`, and `doc` isn't in their keys, so a documentation change stops at the package.

Paths give a first placement, then a file's named things move with it until nothing changes: a
library keeps the data it names, and a document that names a header goes with the headers. A name
that doesn't exist stands for the files in its directory. A package stays whole, with the reason
recorded, when a library names the bare package prefix or a program would land in `lib`. A recipe
can ask for that with `whole = true` when a program finds its files relative to a library at run
time, which no scan can see.

Measured in step 18: `ripgrep`'s closure went from 318 to 54 MiB and the initrd from 149 to 26 MB,
with 5 of 73 packages whole and the same test results as unsplit. The debug output doesn't exist
yet.

### Downloads fetch files with store paths set aside

Objects are stored and fetched by content with store hashes masked, and references are filled in
on install. A comment change in zlib costs 2.2 MiB this way, against 100 MiB as whole
packages. Chunking and compression can come on top.

Measured only. Filling in on install isn't built, and it means installed files no longer match the
fetched objects byte for byte, so verification has to work on the masked form.

### Foreign binaries run in a view

A prebuilt program runs inside a composed `/usr`, unmodified. That keeps it verifiable and needs no
`patchelf`. A launcher can hide the view.

A design view. No foreign binary has been run yet.

### Packages find libraries through `DT_RPATH`

`DT_RUNPATH` isn't searched when glibc dlopens `libgcc_s` for `pthread_exit` and `pthread_cancel`,
so plain C programs abort. It also made `python3`'s `test_ssl` segfault. With `DT_RPATH` both go
away and test results match the unmoved build (step 14).

### A generation is a directory, and switching is a rename

A generation is content-addressed and holds `etc/`, `sw/bin` and `init`. `/system/current` points at
one. `/etc` is an overlay on the generation, so run-time changes vanish at boot.

Firm on the mechanism: a live switch took about 70 ms, restarted only what changed, and survived
power cuts and rollback (step 9).

### A package is named by its contents

A package builds at a temporary path keyed by its recipe and its dependencies' paths. Once built
and stripped, it moves to a path named by a hash of its contents, with references to itself
rewritten. Later packages are keyed on that path, so a rebuild with identical output stops there.
The hash covers every file, link and directory, empty ones too, with their permissions, which
builds make with the umask 022 whatever the host uses.

A zlib comment change rebuilt only zlib, where before it changed all 23 packages above it (step
14). A change inside glibc still changes 64 of 64, because everything holds its path. The system
boots to `running` this way, and moved packages run as well as unmoved ones.

This needs reproducible builds to pay off, and all 64 packages tested reproduce.

Rewriting after the build misses a reference that isn't stored as a string: `ld.so` keeps its own
path in instruction operands, and those still name the temporary path (step 19). Outputs need a
check that the temporary path is gone.

### Everything builds from a seed we packed

`seed.pin` names a 48 MB tarball of our own `gcc`, `binutils`, `glibc` and build tools, packed
from what the previous seed built. Packing it again gives the same files with only store hashes
changed, so the toolchain reproduces itself. Addresses still move with each new seed, because the
seed holds the store paths of the chain that made it. Packing those out would make it a fixed point
(step 19).

### Configuration is actions too

A file, a unit and a generation are actions with records and outputs, the same as packages, run
in-process. Each action has an unresolved key, known before anything runs, and a resolved key.
Where it came from is stored beside it and never hashed, so a worker needs only the record.

A one-line change to a real distro configuration reran 2 of 27 actions and took about 90 ms apart
from an unoptimized package lookup (step 15). Records live in SQLite.

## Speed

| Operation | Time |
|---|---|
| Resolving a package and its hashes, from 100,000 recipes | 1.5 ms |
| A plan after a one-line change, 124 modules | 25 ms |
| A live generation switch | 70 ms |

Recipes stay fast only if they are separate files read on demand and close to plain data. distro's
evaluator is a little superlinear above 2,000 modules, after a fix to a quadratic lookup (step 13). Building
and fetching take most of the real time, and neither has been optimized or measured here.

## An apply is the unit of history

One row per apply or rollback: who (the uid and its name, the person behind `sudo` when root ran
it, and an agent's name from `DISTRO_AGENT`), when, the model hash, the generation, the apply
before it, and digests of the configuration's TOML, its lock and an explanation. The explanation
says which action and part of the model made each path, where each option's value was set, and
what each module read.

Provenance hangs off the apply and never off a generation or an action. Both are named by content,
so two applies can reach the same one with different answers, as when a value moves to another
file. Records and outputs carry no provenance, and the TOML, lock and explanation are kept in
their own directory, apart from the objects generations are made of. So records, outputs and a
generation's objects can be published without the configuration behind them.

Diffs and rollbacks name applies by number and treat the generation only as where an apply
led. A diff cites, for each changed file, the action and part of the model behind it and where
the options its module read were set, as that apply recorded them.

A rollback is a new apply pointing at the old inputs. The TOML stays the only place intent lives,
and `why` and `status` answer about what's running and say when the TOML has moved on.

## Open questions

Decisions the store model still needs:

- Which `/etc` files are state (`passwd`, `group`, `shadow`, `machine-id`) and how they're owned.
- Verifying what's installed, including whether fs-verity can work with filled-in references.
- Garbage collection of objects and packages.

- Where a store can live. Current thinking: `/store` stays the fixed path inside binaries, and the
  bytes can be anywhere via a mount or namespace. Rewriting the root changes what was hashed and
  misses compressed paths, so it needs a security case first.

And things nothing has tested:

- A kernel or bootloader change between generations, or an initrd. The kernel came from QEMU.
- Real hardware, logging in, or PAM. Networking only as far as DHCP in QEMU.
- Test suites against split or trimmed packages, and 14 packages have no test suite at all.
- Several versions of a library in one dev environment.
- macOS, which needs its own packages (Mach-O) on top of anything here.
