# Packaging findings

What went wrong while writing and moving recipes, what each recipe did about it, and what the
tool could do so the next recipe doesn't hit it. Add to it as things come up: one entry per kind
of problem, with the recipes that showed it. When something is done about one, say so in its
status, and move it to the end once it's prevented for every recipe.

Each entry: what happened, what we did then, what would prevent it, and where it belongs: a
style (Starlark), the builder (Rust), or the recipe.

## Open

### gettext's rules for XML files are split from the library that reads them

- **Seen in:** zathura, whose metainfo file `msgfmt` translates.
- **What happened:** `msgfmt` said "cannot locate ITS rules". The rules in
  `share/gettext-0.26/its` went to gettext's programs output, and `libgettextsrc`, in the library
  output, looks for them beside itself; its path to them is put together at run time, so the split
  didn't see the reference.
- **What we did:** zathura sets `GETTEXTDATADIRS` to the programs output's copy.
- **What would prevent it:** keeping `share/gettext-*` with gettext's library. That rebuilds
  everything that uses gettext.

### A package's copy of a system-wide cache

- **Seen in:** GTK 4, which runs `update-mime-database` when shared-mime-info is there, and GTK 3
  with compiled GSettings schemas.
- **What happened:** the system failed on `share/mime/XMLnamespaces` coming from two packages.
- **What we did:** GTK 4's install removes everything in `share/mime` but `packages`.
- **What would prevent it:** the system leaving out a package's copy of what an index makes, or
  the build saying which package made one, and that the system makes it.

### Unit directories from systemd's pkg-config file

- **Seen in:** Waybar, polkit, and any project that asks `systemd.pc` where units go.
- **What happened:** the answer is systemd's own store path, so the install fails, and pkgconf
  doesn't take a `PKG_CONFIG_SYSTEMD_*` override from the environment there.
- **What we did:** a `prepare` line puts `$PREFIX/lib/systemd/user` in place of the lookup.
- **What would prevent it:** systemd's `.pc` giving its directories relative to `${prefix}`, and
  builds defining `prefix` as their own when they read it.

### Zip archives

- **Seen in:** fmt, which releases a `.zip`.
- **What happened:** listing the source with tar failed.
- **What we did:** used the forge's tarball of the tag.
- **What would prevent it:** reading and unpacking zips, by their first bytes.

### No desktop OpenGL library

- **Seen in:** imv 5, which links `gl`, or `opengl` on a system with libglvnd.
- **What happened:** mesa is built with EGL and GLES but without GLX, so it installs no `libGL`
  or `libOpenGL`, and imv's configure stopped.
- **What we did:** left imv out for now.
- **What would prevent it:** libglvnd, with mesa as its vendor. That rebuilds mesa and the whole
  desktop, and the EGL vendor files need a place in the system the loader reads.

### Option names drift between releases

- **Seen in:** libsecret 0.21 (`gir` became `introspection`), libical 4 (`SHARED_ONLY` removed,
  most options renamed with a `LIBICAL_` prefix).
- **What happened:** the build failed at configure, one option at a time, after a full fetch and
  setup.
- **What we did:** read the options from the source archive and renamed them in the recipe.
- **What would prevent it:** the meson style reads `meson.options` (or `meson_options.txt`) from
  the source at plan time, with `recipe.read`, and refuses a `-D name` the project doesn't declare,
  naming the ones it does. The autotools style passes `--enable-option-checking=fatal`. CMake can't
  be checked before it runs; the cmake style passes `--warn-unused-cli`, and a warning about an
  unused variable fails the build.
- **Belongs in:** the styles.
- **Status:** meson done: the style reads `meson.options` or `meson_options.txt` and refuses an
  undeclared `-D`, naming the options the project has, when the recipe is styled. Autotools and
  CMake wait for the next full rebuild, since their flags change every script.

### A configure script finds a compiler that isn't the wrapper

- **Seen in:** libqalculate, whose configure looks for a compiler for the build machine
  (`CC_FOR_BUILD`) by the full target name, `x86_64-pc-linux-gnu-gcc`.
- **What happened:** the compiler it found links programs that can't run in the sandbox, so
  configure stopped with "cannot run C compiled programs".
- **What we did:** set `CC_FOR_BUILD` and `CXX_FOR_BUILD` in the recipe's `env`.
- **What would prevent it:** the wrapper also answers to the target-named compilers
  (`x86_64-pc-linux-gnu-gcc`, `-g++`, `-cc`), so whatever name a build looks for, it gets the
  wrapper.
- **Belongs in:** the builder, in the wrapper and the toolchain.
- **Status:** open.

### A build tool is missing, and only configure says so

- **Seen in:** fuzzel (scdoc, for its man pages), tomlplusplus (cmake, to write CMake package
  files), libsndfile (python, which configure insists on).
- **What happened:** each failed at configure with its own message, and the fix was always one
  word in `extra_deps`.
- **What we did:** added the tool to `extra_deps`.
- **What would prevent it:** when a build fails, the builder matches the end of the log against a
  short table of known messages (`Program 'X' not found`, `no suitable Python interpreter`,
  `Dependency "X" not found`) and says "add `X` to `extra_deps`", naming the recipe that provides
  it.
- **Belongs in:** the builder.
- **Status:** done: a failed build ends with a hint naming the recipe that has the missing
  program, pkg-config module or header, or saying none built so far has it.

### Headers include what the library's .pc file doesn't name

- **Seen in:** libqalculate, whose headers include `gmp.h` and `mpfr.h`.
- **What happened:** noctalia failed to compile partway through with `gmp.h: No such file`.
- **What we did:** `propagates = ["gmp-lib", "mpfr"]` in libqalculate's recipe.
- **What would prevent it:** after a build, the builder already reads the package's `.pc` files.
  It also scans the headers it installed for `#include <...>`, and when one comes from a
  dependency the `.pc` doesn't require, it propagates that dependency or warns that it should be.
- **Belongs in:** the builder, beside what `requires.rs` does now.
- **Status:** open.

### A package of only headers is split away from its own path

- **Seen in:** stb.
- **What happened:** output splitting moved `include/` to the dev output, so `$LAB_DEP_stb`
  pointed at an empty directory.
- **What we did:** `whole = true` in stb's recipe.
- **What would prevent it:** a package whose only content would go to its dev output stays whole,
  or builds get a `$LAB_INC_<name>` that points where its headers went.
- **Belongs in:** the builder, in output splitting.
- **Status:** open.

### Quoting compiler flags in TOML

- **Seen in:** bc and procps (`CC`, `CFLAGS` with spaces), libsndfile (`-std=gnu17`).
- **What happened:** `env` exports values as they are, so a value with spaces needs shell quotes
  inside the TOML string: `"'-O2 -std=gnu17'"`.
- **What we did:** quoted by hand.
- **What would prevent it:** every style takes `cflags` and `cxxflags` as lists, and quotes them
  itself.
- **Belongs in:** the styles.
- **Status:** open.

### GCC 15 compiles C23 by default

- **Seen in:** libsndfile, whose ALAC code defines its own `bool`; unzip, whose K&R
  declarations such as `struct tm *localtime();` now mean no arguments.
- **What happened:** `bool`, `true` and `false` are keywords in C23, and `()` declares no
  arguments where it used to mean any, so the code didn't compile.
- **What we did:** `-std=gnu17` for that recipe.
- **What would prevent it:** nothing general; old C code needs it said per recipe. With the
  `cflags` option above, it's one line. Worth noting so the next one is recognised quickly.
- **Belongs in:** the recipe.
- **Status:** known.

### A Rust project's repository has no lock, but its published crate does

- **Seen in:** cargo-c.
- **What happened:** the cargo style found no `Cargo.lock` in the GitHub archive. The crate on
  crates.io has one, since crates with programs publish their lock, but its version carries build
  metadata (`0.10.26+cargo-0.100.0`) that the release tag doesn't.
- **What we did:** took the source from `static.crates.io` with the full version.
- **What would prevent it:** a cargo style option, `crates_io = true`, that takes the source from
  the registry by the recipe's id and version, so the recipe doesn't spell the URL.
- **Belongs in:** the cargo style.
- **Status:** open.

### A file moves inside a project that has no releases

- **Seen in:** stb, where `stb_image_resize2.h` moved into `deprecated/` at the commit we took.
- **What happened:** the recipe installed every header at the top, so the one noctalia needed was
  missing, and the error looked like a search path problem.
- **What we did:** copied it from `deprecated/` too.
- **What would prevent it:** a `smoke` line for the headers its users need, so the recipe
  fails where the file is missing and not two packages later.
- **Belongs in:** the recipe.
- **Status:** known.

### Reading a project's real options takes digging

- **Seen in:** libsecret, libical, noctalia.
- **What happened:** each time, the options had to be read by hand from the source archive in the
  store.
- **What would prevent it:** `distro info <package>`, which would list a project's options from
  `meson.options`, `configure --help` or the CMake cache, for whoever writes or updates a recipe.
- **Belongs in:** the builder's tools.
- **Status:** open.

### A build tool calls another, which reads its configuration from elsewhere

- **Seen in:** librsvg, whose Meson build runs Cargo and cargo-c from its build directory.
- **What happened:** Cargo looks for `.cargo/config.toml` from where it runs, which isn't the
  source, so the vendored crates weren't found.
- **What we did:** the cargo style's `vendor_only` writes the configuration to `$CARGO_HOME` too,
  with absolute paths and `offline = true`.
- **Status:** done, for Cargo. Other tools that nest the same way will need the same.

### Machines without a supported GPU

- **Seen in:** noctalia in the VM tests.
- **What happened:** Mesa had only the AMD driver, so EGL failed to start without an AMD GPU and
  the shell exited.
- **What we did:** added softpipe, which draws without a GPU.
- **Status:** done.

### A build tool that runs another doesn't bring it

- **Seen in:** libnl, libpcap and tmux (bison and flex), mtr (autoconf's autom4te), which all run
  m4 as they work.
- **What happened:** each failed with m4 missing, or with a misleading "flex not found", though
  the tool itself was there.
- **What we did:** listed `m4-final` in each recipe.
- **What would prevent it:** bison, flex and autoconf propagate m4, so whatever uses them has it.
  That rebuilds everything built with them, so it waits for the next rebuild of everything.
- **Belongs in:** those recipes.
- **Status:** open.

### aclocal doesn't look in the build's /usr

- **Seen in:** mtr, whose release has no configure, so the build makes it.
- **What happened:** aclocal searched only automake's own directory, so pkg-config's macros in
  pkgconf's dev output weren't found, and configure failed on `PKG_PROG_PKG_CONFIG`, which the
  hint then called a missing program.
- **What we did:** `ACLOCAL_OPTS="-I /usr/share/aclocal"`, where the build's view has every
  dependency's macros. The hint now says when a name is an unexpanded macro.
- **What would prevent it:** builds have `ACLOCAL_PATH=/usr/share/aclocal`, with the next rebuild
  of everything.
- **Belongs in:** the builder's environment for builds.
- **Status:** open.

### A program fetches more sources as it builds

- **Seen in:** helix, which clones each language's tree-sitter grammar from git while it builds.
- **What happened:** the sandbox has no network, so the grammar step would fail.
- **What we did:** `HELIX_DISABLE_AUTO_GRAMMAR_BUILD`, so helix has themes and queries but no
  grammars, and highlights nothing.
- **What would prevent it:** a style option that reads `languages.toml` and adds each grammar's
  repository and commit as a git source, as the cargo style does for git crates.
- **Belongs in:** a style for helix's grammars, or the cargo style's options.
- **Status:** open.

## Prevented

### A plan fetched every recipe's sources

- **Seen in:** applying to the Framework after adding about 50 recipes.
- **What happened:** to make the plan, every styled recipe's source was fetched for its lock, and
  every package's sources for its key, including recipes the configuration never used. With
  nothing printed, it looked hung.
- **What we did:** plans and builds start from what the configuration names and style only the
  recipes they reach; keys take hashes from `sources.lock` or a lock without fetching, and each
  download prints a line.

### Archives with no directory around their files

- **Seen in:** Nerd Fonts, whose release archives hold the font files at the top.
- **What happened:** unpacking stripped the first path component, so every file was dropped.
- **What we did:** the top directory is stripped only when everything in the archive is under one.

### Archives that list their directory without a slash

- **Seen in:** OpenSSH, whose release lists `openssh-10.0p1` before `openssh-10.0p1/...`.
- **What happened:** the check for a top directory wanted a slash on every line, so the source
  was unpacked with its directory around it and `./configure` wasn't found.
- **What we did:** the directory's own entry counts, with or without its slash.

### An archive that holds a file twice

- **Seen in:** fish, whose release has a newer `Cargo.lock` appended after the first.
- **What happened:** reading the lock out of the archive gave both copies one after the other,
  which isn't TOML, while unpacking leaves only the last.
- **What we did:** the lock is read from the last copy, as unpacking leaves it.

### An install outside the prefix

- **Seen in:** wl-clipboard, which installs fish completions where fish's pkg-config file says,
  `/usr/share/fish`, inside the build's read-only `/usr`. Waybar, whose user unit went to
  systemd's own store path, which Python reports as `OSError: [Errno 30]`.
- **What happened:** Meson failed with "Unhandled python OSError" and nothing else to go on.
- **What we did:** a failed build that last installed outside `/store`, or wrote to a read-only
  place, says so and to point that directory's option under the prefix.

### `.pc` lines that go on over several

- **Seen in:** Poppler, whose `Requires.private` ends its lines in backslashes.
- **What happened:** the check of what a `.pc` file requires read `\` as a module.
- **What we did:** a line ending in a backslash is joined to the next, as pkg-config reads it.

### Meson looking for a fallback subproject

- **Seen in:** GTK 4 without `wayland` listed, where Meson tried to fetch it as a subproject.
- **What happened:** the failure said git wasn't found, which doesn't say what's missing.
- **What we did:** the hint reads the dependency from the fallback line and names the recipe that
  has it.

### A sign-in page pinned as a source

- **Seen in:** slurp, whose GitLab archive URL names a project that isn't there. Also
  code.videolan.org, whose bot check answers every archive URL with a page.
- **What happened:** GitLab answered with its sign-in page and a success status, so the page was
  pinned in `sources.lock` and the meson style read it as a project with no options.
- **What we did:** a download that starts as an HTML page is refused and not pinned.

### A project's own `vendor` directory

- **Seen in:** atuin, which keeps a patched crate in `vendor/axoasset`.
- **What happened:** the cargo style vendored crates into `vendor/` too, and Cargo read atuin's
  crate as one of ours and failed for want of its checksum file.
- **What we did:** crates go in `.cargo-vendor/`.

### Data a library refers to was missing from the system

- **Seen in:** fontconfig, whose presets went unused, and libX11, whose compose tables xkbcommon
  couldn't find, so dead keys didn't work in niri.
- **What happened:** splitting moves data a library refers to into the library output, with the
  library, and a system linked only each package's main output into what it shares.
- **What we did:** a system shares the `share` of a package's library output too.

### A helper compiled in by its store path, which can't be setuid

- **Seen in:** Linux-PAM, whose `pam_unix` checks a password for someone who isn't root, such as
  noctalia's lock screen, by running `unix_chkpwd`.
- **What happened:** the helper's path was its copy in the store, which isn't setuid, so it couldn't
  read `/etc/shadow` and every password was refused.
- **What we did:** the recipe points `pam_unix` at the system's setuid wrapper in
  `/run/wrappers/bin`.
- **What would prevent it:** a check that no package names a store path to a program the system
  wraps setuid.

### A crate from git joined the project's workspace

- **Seen in:** yazi, which patches `ratatui-core` from a fork on GitHub.
- **What happened:** the checkout was under the source, and Cargo makes every path dependency
  inside a workspace a member of it, so the crate inherited yazi's workspace settings in place of
  ratatui's, and one it needed was missing.
- **What we did:** git checkouts go in `git/` beside the source. yazi also patches the crate in
  place of the one from crates.io, and Cargo doesn't patch a patch, so the cargo style reads the
  project's `Cargo.toml` and patches crates.io for those.

### A system library only the lock names

- **Seen in:** atuin, whose lock has `openssl-sys`.
- **What happened:** the build failed at the crate's build script, looking for OpenSSL.
- **What we did:** the cargo style brings the library and pkg-config for each such crate its
  lock names, from a table: `openssl-sys` and `libdbus-sys` so far. gitui also asked for its own
  OpenSSL built from source, which needs Perl; the style sets `OPENSSL_NO_VENDOR` so the system's
  is used.

### Fonts drawn without hinting or fallbacks

- **Seen in:** noctalia and alacritty on the Framework.
- **What happened:** `/etc/fonts/fonts.conf` named the font directory and nothing else, so none of
  fontconfig's presets for antialiasing, hinting and fallbacks applied, and DejaVu was the only
  face.
- **What we did:** the fonts module includes the presets, installs Inter, JetBrainsMono Nerd Font
  and Noto Color Emoji, and sets the generic families, as options.

### Crates from git, fetched as GitHub archives

- **Seen in:** niri's Smithay.
- **What happened:** only GitHub worked, submodules were missing, and an archive's hash could
  change if GitHub changed how it compresses.
- **What we did:** git sources, cloned at the commit from any host and pinned by their files
  (decision 0028).

### Library names written back to back

- **Seen in:** alacritty, whose binary holds `libEGL.so.1libEGL.so` with no NUL between them,
  since Rust's strings have none.
- **What happened:** the run path scan read one long name and dropped Mesa from the run path, so
  EGL couldn't be loaded.
- **What we did:** a name ends after its version numbers.

### No certificate authorities

- **Seen in:** curl and wget on the Framework.
- **What happened:** nothing provided a CA bundle, so every TLS connection failed.
- **What we did:** every system has Mozilla's bundle where programs look for it.

### The compiler in every C++ program's closure

- **Seen in:** shared-mime-info, and through it every system with GTK 3 or 4.
- **What happened:** libstdc++'s headers put their own paths in assertion messages through
  `__FILE__`, so programs referred to gcc's output, and its 251 MiB came with them.
- **What we did:** the compiler wrapper maps gcc's `include` to `/usr/include` in every compile
  (`-fmacro-prefix-map`).

### A library without a SONAME

- **Seen in:** sqlite 3.54, whose new build system leaves the SONAME out unless given
  `--soname=legacy`, and zathura, which linked it.
- **What happened:** zathura recorded `/usr/lib/libsqlite3.so`, the build's path, as a library it
  needs, and couldn't start anywhere else. Nothing noticed until its smoke command ran.
- **What we did:** sqlite builds with `--soname=legacy`, and a build whose programs need a library
  by a path outside the store fails, naming the file and the path.

### Smoke commands nobody ran

- **Seen in:** cliphist, slurp, swayidle, tmux and GTK 4, whose smoke commands used flags the
  programs don't have or needed a display or a locale.
- **What happened:** only the experiments ran smoke commands, so these were never tried.
- **What we did:** `distro build PKG --check` runs them, and they were fixed.

### A reference to a bare `lib` follows the libraries

- **Seen in:** gobject-introspection, whose `g-ir-scanner` falls back on its prefix's `lib`, joined
  with `gobject-introspection` at run time, for its Python module.
- **What happened:** splitting pointed `<prefix>/lib` at the part with the shared libraries, but
  the Python module stayed in the main part, so the scanner couldn't import itself.
- **What we did:** `whole = true` for the recipe.
- **What would prevent it:** a reference that names a directory both parts have, with nothing
  after it, could point at the main part, or keep the package whole with a note.
- **Belongs in:** splitting.
- **Status:** worked around.

### glibc's ldd looks for the loader in lib64

- **Seen in:** gobject-introspection's scanner, which runs `ldd` on a program it builds; and `ldd`
  on every machine, which says "not a dynamic executable" for everything.
- **What happened:** glibc's `ldd` script lists `<prefix>/lib64/ld-linux-x86-64.so.2` in
  `RTLDLIST`, but our glibc installs the loader in `<prefix>/lib`.
- **What we did:** the scanner runs the loader with `--list`, which prints the same.
- **What would prevent it:** fixing `RTLDLIST` in the glibc recipe, with the next change that
  rebuilds everything anyway; and a smoke command such as `bin/ldd bin/ldconfig`.
- **Belongs in:** the glibc recipe.
- **Status:** known; worked around in gobject-introspection.

### Two packages with runtime dependencies couldn't share a build

- **Seen in:** Ghostty, which builds with gtk4 and blueprint-compiler.
- **What happened:** each package with `runtime_deps` records them in `lib/distro/runtime-deps`,
  so a build view with two of them had one file from two packages, which is an error.
- **What we did:** views leave out `lib/distro`, which builds have no use for.
- **What would prevent it:** nothing more; the view test covers it.
- **Belongs in:** the builder.
- **Status:** fixed.

### Zig links past the compiler wrapper

- **Seen in:** Ghostty, whose program got a run path with GTK's directory alone, so it couldn't
  find freetype.
- **What happened:** the wrapper gives C and C++ programs the run paths of what they're built with;
  `zig build` runs its own linker.
- **What we did:** the zig style sets each ELF file's run path from `LAB_LINK_FLAGS` after the
  build, and the build trims it as usual.
- **What would prevent it:** the same step in any style whose tool links by itself.
- **Belongs in:** the style.
- **Status:** fixed for zig.

### bzip2 has no pkg-config file

- **Seen in:** Ghostty, which asks pkg-config for `bzip2` when it links freetype from the system.
- **What happened:** bzip2 1.0.8 ships no `.pc`, so Zig didn't find it.
- **What we did:** the recipe writes one for the build.
- **What would prevent it:** bzip2's recipe installing a `bzip2.pc`, as distributions do; it's in
  the toolset, so that waits for a change that rebuilds everything anyway.
- **Belongs in:** the bzip2 recipe.
- **Status:** worked around in Ghostty.

### Python packages a build uses are put on its path by hand

- **Seen in:** gobject-introspection and glib's typelibs needing setuptools, as mesa needs mako
  and systemd jinja2.
- **What happened:** a dependency's `site` isn't on `PYTHONPATH` unless the recipe says so, so
  meson didn't find setuptools though the recipe listed it.
- **What we did:** `env = { PYTHONPATH = "$LAB_DEP_setuptools/site" }`, as the others do.
- **What would prevent it:** the builder adding every dependency's `site` to `PYTHONPATH`, as it
  does for `bin` on `PATH`.
- **Belongs in:** the builder.
- **Status:** known.

### Prebuilt programs need every library and function they link, used or not

- **Seen in:** Zen's build from its project, on a Wayland-only GTK 3.
- **What happened:** its libraries are linked with `BIND_NOW` against GDK's X11 functions and
  eight X11 extension libraries, so it can't start on a GTK 3 without its X11 backend, even on
  Wayland. GTK 3's X11 backend then needed cairo with Xlib and libepoxy with GLX.
- **What we did:** the X11 libraries, and GTK 3, cairo and libepoxy built with X11 beside Wayland,
  which rebuilt what's above cairo once.
- **What would prevent it:** a check that a prebuilt recipe's every needed library resolves and
  every undefined function is defined by one, at build time, so a missing one is an error
  naming it rather than a program that won't start on the machine.
- **Belongs in:** the builder, for recipes that patch prebuilt programs.
- **Status:** checked by hand for Zen.

## Working on recipes

- Don't edit `build/` while a build is running from it. Styles load once per process, so a build
  can pair an old style with a new recipe and fail in ways that have nothing to do with either.
- TOML 1.0 has no line breaks inside an inline table: use a sub-table, such as
  `[package.cargo.env]`, for anything longer than a line.
- Starlark's `r"..."` strings don't keep backslashes as Python's do: write `\\` in an ordinary
  string.
