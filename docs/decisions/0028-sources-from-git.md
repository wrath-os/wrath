# 0028: Sources from git

Status: accepted, 2026-11-11.

## Context

Every language's lock can take dependencies from git as well as from its registry, and the lock
pins a commit but no hash of the contents:

- Cargo: `source = "git+https://github.com/Smithay/smithay.git?rev=...#<commit>"`, with no
  `checksum`.
- Go: the module proxy serves almost everything, and `go.sum` vouches for it. Modules it can't
  serve, such as private ones or forks named in a `replace`, come from git.
- npm: `"resolved": "git+https://...#<commit>"`, with no `integrity`.
- Python: `uv.lock` and `poetry.lock` record a git URL and commit.
- Ruby: a `GIT` section in `Gemfile.lock` with a `revision`.

The cargo style fetches GitHub's archive of the commit and `sources.lock` pins the archive's hash
when it's first fetched. That works for niri, but:

- GitHub's archives aren't byte-stable. Its 2023 change of compression changed the hash of every
  archive, which broke every build that pinned one.
- Only GitHub has a known archive URL. GitLab, Codeberg, sr.ht and a plain git server don't
  work, and archives leave out submodules.
- The recipe has to say where in the repository each crate is, and the `[patch]` that points
  Cargo at them changes the lock, so the build can't be `--locked`.

Nix needed a hash for each git dependency written into the recipe (`outputHashes`), changed by
hand with every update, or else fetched git while evaluating, which made evaluation impure.

## Decision

- A source can be a git commit, `git+https://...#<commit>`. The fetcher clones only that commit,
  with its submodules, from any host. The commit is its full hash. A repository can't start
  with `-`, a submodule can't be on this host (`file://`), and the host's git settings aren't
  read, since a lock from upstream names what's fetched.
- What's pinned is the hash of the checked-out tree, not of an archive, so it doesn't change with
  how a host compresses. `sources.lock` pins it on the first fetch, as it does now for
  sources without a hash. No recipe carries one.
- Styles emit git sources for what a lock takes from git, and lay them out as the language's own
  tools expect. For Cargo, the build finds each crate by name among the manifests in its
  repository and patches it in, so the recipe names no paths. Go, npm, Python and Ruby styles do
  the same when they come.
- A Cargo build with crates from git isn't `--locked` yet: the patch changes the lock's sources.
  Vendoring them as `cargo vendor` does would keep it, but needs their manifests rewritten with
  the workspace's inherited fields, which only Cargo does well. It stays `--offline`, so it can
  only use what the lock and the pinned commits hold.
- A commit that moved, a repository that's gone or a tree whose hash changed is an error at
  fetch, naming the source and the lock entry it came from.

## Consequences

- niri's recipe names no paths, and Smithay comes from a clone of the commit, not GitHub's
  archive.
- Every future style handles git dependencies the same way. A style that can't lay out a git
  dependency for its language fails with a message naming it, instead of a half-working build.
- A git dependency is checked once, by the pinned tree hash. Registry dependencies are still
  checked twice, by the lock's hash and by the pin.
- The fetcher gains a git client. Fetching stays outside the sandbox, before any build, as it is
  for every source.
