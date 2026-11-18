# 0038: A binary cache in a bucket

Status: accepted, 2026-11-15; the first step is built.

## Context

A world rebuild takes 50 minutes on 32 cores, and a machine that builds for itself starts with
the toolchain. All four reviewers named a binary cache as what's missing. 0018 says a cache maps a
key to the paths it built, signed by the builder; `next.md` section 1 refined that: look up by
the resolved key, keep traces append-only, and let files go up unsigned, since their addresses
check them.

What's left to settle: whether a bucket, plain object storage behind the S3 API, is enough, given
that Nix, Bazel and Buck2 each come with more machinery.

## What the others run, and what for

- Bazel can use a plain HTTP cache: `GET` and `PUT` on `/ac/<digest>` and `/cas/<digest>`, served
  by nginx or bazel-remote. Its gRPC protocol (REAPI) is for remote execution, a scheduler and
  workers, plus `FindMissingBlobs`, which asks about thousands of digests in one request. Entries
  in its action cache aren't signed, so the server decides who may write them.
- Buck2 speaks only REAPI, because it was built around remote execution.
- Nix uses S3 as a cache directly: a `narinfo` per path, signed, next to the archive. Cachix and
  Attic add push tokens for many users, garbage collection, chunks shared across versions of a
  path, and one query for which of many paths are missing.

So the machinery buys five things: remote execution, batched queries, the server deciding who
writes, garbage collection, and sharing data across versions. For wrath:

- Remote execution isn't wanted now, and doesn't need to share a format with the cache.
- Signatures decide what a machine trusts, as in Nix. Write credentials to the bucket go only to
  whoever builds for it.
- A bucket has no batched queries. We batch when writing instead: one signed index per grapes
  revision lists every recipe's key and outputs, so a machine reads one object.
- A bucket has no idea what's reachable. `wrath cache clean`, run by whoever can write, walks
  from the indexes it keeps, and deletes only what's older than a grace period.
- Sharing data across versions (0018's per-file manifests) is done by the client and needs no
  server either; it comes later.

## Where a bucket could be slow

- Round trips. A `GET` costs 20 to 50 ms within a region, and 100 to 200 ms from home. A resolved
  key depends on its dependencies' addresses, so walking the graph by keys costs a round trip per
  level, in parallel within each level. The per-revision index removes that for everything grapes
  has.
- Small objects. Per-file storage would mean thousands of requests for one package, each with
  its latency and its fee. A path is one archive to start with, as `build/work/raw` keeps them.
- Egress. One world is about 5 GB compressed (`build/work/raw` holds 10 GB for about two). That
  costs about $0.45 to download from AWS S3. Cloudflare R2 charges nothing for egress, and Garage or
  MinIO at home charges nothing at all.
- Pushing checks what's there with a `HEAD` per path, or skips that with a conditional `PUT`
  (`If-None-Match: *`) where the store supports it. That's a few hundred requests per world.
- Listing for `clean` reads 1,000 keys per request. That's fine for a weekly run.

S3 has been strongly consistent since 2020: once a write returns, every read sees it. A reader
that sees a trace finds its paths too, as long as the trace is written last.

The verdict: a bucket is enough for us. What it lacks, queries and reachability, moves to the
writer.

## Decision

One layout, served from a directory (`file://`), any static web server or a public bucket URL
(`https://`, read only), or the S3 API (`s3://`, read and write; AWS, R2, B2, Garage, MinIO):

```text
info                            format version and hash names
paths/<address>.tar.zst         a store path, as raw/ archives one today
traces/<key>/<rules>/<signer>   signed: each output's path, contents digest and references
hints/<unresolved key>          unsigned: the resolved key it last built as
sets/<grapes revision>/<target> signed: every recipe's keys and outputs at that revision
sources/<sha256>                source archives as fetched, for when upstream moves them
```

**A trace** is one build: under the build key (dependencies named by their addresses) and the
fix-up rules' digest, it lists each output's store path, a digest of its contents and the paths it
refers to. The references are there so a machine can fetch only the runtime closure without
another request per path. Each signer writes its own object, and nothing is ever overwritten. Two
signers that disagree on one key are a reproducibility report.

**A set** is every trace for a grapes revision and target in one signed object. CI publishes it
after building the world at a revision.

**Fetching.** Before building, a machine:

1. reads the set for the grapes revision its lock pins, if there is one, and checks its signature;
2. works out each key itself, from the dependencies' addresses the set gives, so the set only
   saves requests and never has to be believed about which key goes with which recipe;
3. looks up the trace for what the set lacks, such as variants, a configuration's own builds or a
   recipe changed in a checkout, level by level;
4. downloads the paths it doesn't have, in parallel, and checks each one. A content-addressed path
   is hashed again and must match its name. A boot package, which keeps the path its key gave it,
   must match the contents digest in the signed trace;
5. builds whatever is still missing.

**Pushing.** `wrath cache push` (or `build --push`) uploads paths first, then the trace, then the
set. Raw outputs stay local; a machine that fetches has none, as 0037 says.

**Trust.** A machine lists the caches it reads and the public keys it accepts, ed25519. One key
signs to start with; requiring K of N signers later reuses the per-signer traces as they are.

**Cleaning.** `wrath cache clean --keep N` keeps the last N sets per target, and every trace
written in the last month. It also keeps the paths these refer to, and whatever is younger than a
day, so a push that is still running survives. Everything else goes.

## Consequences

- A cache needs no server of ours. A directory works too, which is also how the first one runs:
  this machine pushes into `build/work/cache`, and the Framework reads it over HTTP on the
  tailnet.
- Machines never see raw outputs. A change to fix-up rules makes traces under the old rules
  useless to a machine with the new ones; CI pushes again after such a change.
- Boot packages are trusted by signature alone, since their names don't come from their
  contents. The toolchain is the part every machine shares, so it's also the part most worth
  making content-addressed later.
- A key that wrath works out differently on two machines, such as from a host path leaking into a
  build, shows up as a cache miss.

## Built so far

The first step, 2026-11-15: traces and paths as above, in a directory, read from a directory or
over HTTP. `wrath cache key` and `wrath cache push DIR`, which pushes every build the work
directory keeps under the current fix-up rules; global `--cache` and `--cache-key` flags in place
of the configuration's side. A build looks up each package before building it, in graph order, so
a machine fetches the whole build graph, toolchain included, rather than only the runtime closure.
The seed is unpacked only when something builds, so a machine that fetches everything never needs
it. Tried on this machine: pushing the world took 39 seconds and 5.7 GB, and a fresh work
directory fetched jq and ripgrep over HTTP in 3 seconds, at the same store paths as here. A VM
booted from an image did the same in 4 seconds, over its network from this machine. Fetching is
wrath's own code (ureq, zstd and tar crates), as is fetching a source such as grapes, so a machine
needs no curl, tar or zstd; pushing and recipes' source downloads still run curl, tar and git,
which only builders need. Not yet: sets, hints, sources, S3, `cache clean`, checking a
content-addressed path against its own name.

Then `[caches.NAME]` with `url` and `keys`: a system's configuration lists caches and trusts
keys, a project lists caches by URL only, and `--cache` and `--cache-key` add to them for one
command. A cache with no key trusted anywhere is warned about once. `wrath cache key` prints the
table to paste, with `--url` for where machines fetch from. A project trusts only what the command
line gives so far; reading the keys of the machine's own configuration for it is still to do.

Then fetching only what runs. A trace lists, for each path, what its dependents' keys read of it:
its directories, the files directly in `lib` and its `.pc` files. A build works out keys down the
graph from traces alone, and a path comes into the store when something runs it, or when a build
that missed the cache builds with it. Fetching jq and ripgrep into a fresh work directory went from
1.6 GB in 3 seconds to 102 MB in under one, at the same store paths; in a VM, from 13.7 seconds to
2.6. A trace from before listings fetches its paths with it, as before. A path whose archive turns
out missing or wrong only shows when it's needed, and fails the command then.

Then plans say what getting there takes: a dry run works out every key from the store and the
caches' traces, fetching traces and nothing else, and counts the paths that would come from each
cache, with the archives' sizes, and the packages that would be built. A system plan from a fresh
work directory takes about a quarter of a second. A cache that can't be reached is warned about
once and left out for the rest of the command, and `--offline` asks none.

Then the rest of the first list. A plan names what no cache has and counts what's built only
because it builds with that; `wrath cache check PKG` follows one package. Fetching shows one line
on a terminal and a total when it's done, and only programs and libraries of what was asked come
along, which took a VM's apply from 41 paths to 5. On a machine wrath installed, the builder works
in `/wrath/work`, whose `store` links to `/wrath/store`, and the `wrath` package carries its own
source in `share/wrath`, so wrath there has the tree it was built from and finds its key. A cachetest
VM applied its configuration with jq added, fetched 5 paths (1.7 MB) into its store and switched in
3.5 seconds.

Then what an adversarial review found. A signature covers the trace's bytes as sent, after
`wrath-trace-v1\0`, so a field a later wrath adds doesn't break it; every older trace is refused,
and a cache is pushed to again. A verified trace is kept in the work directory with where it came
from and who signed it, and a path is fetched only after that key is checked again, from any cache
in use that has its archive. A path no cache can give takes its trace and fix-up record with it,
so its package is built the next time instead of failing for good, and `--offline` fetches
nothing. Keys of one name are all tried, so a key can be replaced. A cache that answers 403
lacks the object, as a bucket says it; a lost connection or a server's error is tried twice more
before the cache is left out. Downloads may take as long as their size needs, and no more bytes
than the trace says. Names in a trace must be store paths, unpacking clears setuid and setgid,
an interrupted unpack leaves nothing in the store, and two commands fetching the same path both
succeed. A plan takes no build locks, counts what builds of missing packages build with and
whether the seed needs preparing, and a cache URL wrath can't fetch from is an error in the
configuration. Still open from the review: fetching in parallel, outside the lock builds share;
keys reading through links into `/wrath/store`, which resolve on a machine and not on a builder;
trust scoped per cache; and normalized archives.

## Open

- Who hosts the first public cache, and what it costs: R2 for egress, or a box at home.
- Per-file manifests and content-defined chunks (0018), once paths are fetched often enough for
  the bandwidth to matter.

## Alternatives

- A server of our own with a query API, as Attic has. It's faster for a walk without a set, but
  it's a service to run, secure and keep up, for the one case the set doesn't cover.
- REAPI, to share Bazel's and Buck2's caches and tools. It's a large protocol meant for remote
  execution, and its entries aren't signed, so the server would have to decide who writes.
- A signed entry per path as the whole layout. A path would be verified only by signature, and
  without traces there'd be no telling why two builders disagree.
