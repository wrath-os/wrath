# The next full rebuild: what goes in it

Everything below changes package keys or what builds see, so it lands together and the world
rebuilds once. Each item says what's wrong, what others do, the fix, and how it's checked. Items
marked "no rebuild" can land any time.

## Builder

### 1. Keys cover the whole recipe

`recipe_parts` picks a package's fields by hand, and missed a source's `into` and `file`, so
moving a source within the tree reused the old build.

Nix hashes the whole derivation, every input and attribute, and Bazel hashes the whole action.
Both are right to: a key that leaves out by default can't be trusted.

Fix: `recipe_parts` destructures `Package` and `Source` without `..`, so a new field doesn't
compile until it's either keyed or on a short list of fields that don't change what's built
(`smoke`, descriptions). `into` and `file` go in.

Check: a test per keyed field that changing it changes the key.

### 2. Golden tests for what the builder does after a build

`BUILD_RULES` has to be bumped by hand when run paths, splitting, rewriting or addressing change.
It was missed once, and Firefox's broken build was reused.

Fix: small fixture packages (an ELF with a run path, a file with the package's path whole and
split, a `.pc`, a header) run through placing and splitting, compared with golden output. A change
to that code changes the goldens, and the failing test says to bump `BUILD_RULES`.

### 3. Rewrite a package's own hash, not hash and name

glib and libqalculate keep their temporary path. glib's `_g_locale_get_charset_aliases` builds
`<libdir>/charset.alias` with four 16-byte copies at offsets 0, 16, 32 and 47; the last overlaps
the third by a byte, so the binary holds `/store/<hash>-` and `-glib-2.84.4/lib`, and a search for
`<hash>-glib-2.84.4` misses it. libqalculate's locale directory is split the same way. The hash is
whole in both; only the name after it is broken. The effect is small (an obsolete file glib does
without, and libqalculate's translations), but the path is wrong.

The temporary and final paths end in the same name, so only the 40-character hash needs replacing.
Nix rewrites hash parts for the same reason.

Fix: after the whole-path rewrite, rewrite any bare hash left, and mask those places when
computing the address. Which part of a split package a cut-apart path means can't be told, so such
a package stays whole, with that as the reason. The split-piece check stays for cases where the
hash itself is split (glibc's inlined `memcmp`), which nothing can rewrite safely.

Check: a fixture with the path copied in overlapping 16-byte pieces; glib's and libqalculate's
warnings gone.

### 4. `WRATH_` in builds, not `LAB_`

`LAB_` is left from when the builder was an experiment called the lab. Recipes and styles see
`LAB_ARCH`, `LAB_OS`, `LAB_TRIPLE`, `LAB_LOADER`, `LAB_SEED`, `LAB_REAL_BIN`, `LAB_LINK_FLAGS`,
`LAB_ARM`, `LAB_DEP_<name>` and `LAB_LIB_<name>`, about 75 uses.

They become `WRATH_`: the builder promises them to any set. A prefix stays, since `ARCH` and
`TARGET` are read by the kernel's and others' Makefiles. Host API stays 1: nothing is published.

### 5. The builds' environment brings what tools look for

Three findings are the same gap: a dependency is in the build's `/usr` but the tool that needs it
isn't told.

- aclocal searches only automake's directory (mtr). Builds get `ACLOCAL_PATH=/usr/share/aclocal`.
- A dependency's Python packages aren't on `PYTHONPATH` (mesa, systemd, glib's typelibs). Builds
  get every dependency's `site` on it, as `bin` is on `PATH`.
- configure looks for `x86_64-pc-linux-gnu-gcc` and finds a compiler that isn't the wrapper
  (libqalculate). The wrapper answers to the target-named compilers too.

Here the builder does it, in one place, for every build, rather than each package injecting code
into its dependents' builds.

Then the recipes' own `ACLOCAL_OPTS`, `PYTHONPATH` and `CC_FOR_BUILD` go.

### 6. Splitting keeps together what belongs together

- A package whose only content would go to its dev output (stb, headers only) stays whole.
- A reference to a bare `<prefix>/lib` can't be told from a run path, which every program has,
  so gobject-introspection's scanner keeps its explicit `whole = true`.

Then stb's `whole = true` goes.

### 7. Checks that fail at build time, not on the machine (no rebuild)

- A prebuilt package: every library it needs resolves, and every undefined function is defined by
  one (`ldd -r` in the sandbox, now that ldd works). Checking functions as well as libraries
  catches what bit Zen.
- Headers a package installs that include a dependency's header its `.pc` doesn't name
  (libqalculate's `gmp.h`): a warning naming the dependency to propagate, logged, for this rebuild;
  an error once the log shows it's right. Explicit, rather than the builder propagating on its
  own guess.
- `wrath build no-such-package` says so before building anything.
- No temporary path survives placing. After the rewrite, the package's temporary hash is looked
  for everywhere, inside gzip, xz and zstd files too, where the rewrite can't reach and the path
  is left naming nothing (Nix's content-addressed builds hit this with compressed man pages). A
  hit is a warning naming the file, logged with the split pieces; the recipe or style then stops
  compressing it, or says why it's harmless. Man pages are already plain in 1241 of the 1243
  packages that have them, so few should show up.

## Recipes and styles

### 8. bison, flex and autoconf bring m4

They run m4 at run time, so they propagate it (nixpkgs does the same). Then the `m4-final` in
libnl, libpcap, tmux and mtr goes.

### 9. Styles take flags as lists

`cflags` and `cxxflags` lists in every style, quoted by the style, replace hand-quoted `env`
strings (bc, procps, libsndfile). Old C under GCC 15 is then `cflags = ["-std=gnu17"]`.

### 10. Styles say a target their own way

Go spells x86_64 `amd64`, gcc's `-march` says `x86-64`, Rust and LLVM have their own names.
Styles translate `WRATH_ARCH` into theirs (the go style sets `GOARCH`), and the lint also flags
`amd64`, `x86-64`, `x64` and `arm64` written in recipes.

### 11. Small ones

- cargo style `crates_io = true`: the source from the registry by id and version (cargo-c).
- helix's grammars: a style option reads `languages.toml` and adds each grammar's repository and
  commit as a git source, as the cargo style does for git crates. nixpkgs reads the same file.
- stb: a `smoke` line for the headers its users need.

## Grapes and the tool (no rebuild)

### 12. Grapes is implied, and the lock pins it

A configuration that names no grapes builds on `wrath-os/grapes`, as it builds on grapes' modules
without naming them. The first command that needs it pins it in the lock like any source, so
`wrath update grapes` and rollback work, and the TOML stays clean.

The pin is written the moment it's used, and nothing global decides it.

When `--source` or `WRATH_GRAPES` replaces the pin, plan and build say so: "grapes from ../grapes,
not the pinned version". The `share/wrath/modules` fallback goes, leaving one way to find modules.

### 13. Determinism checks the set in use

`wrath dev test --determinism` with no path checks the modules of the set in use, so the
acceptance case doesn't name a directory. Needs an acceptance edit, reviewed.

### 14. The redundant-dependency report is information

It was added on 2026-11-13 and used once to trim seven recipe files; no broken build was traced to
a redundant listing. Listing what a package uses directly is right even when another dependency
brings it, as Cargo asks: it keeps working when that other dependency stops bringing it, and
prebuilt programs like Firefox need what they load listed. The report moves behind
`wrath dev lint --redundant` and doesn't fail lint.

## Left out

- `wrath info <package>` listing a project's options: useful, separate, no rebuild.
- GCC 15 and C23: per recipe, made a one-liner by 9.
