# 0009: Sources, and including files from them

Status: accepted, 2026-11-07.

## Context

A source is where packages come from: a name and the URL it follows, pinned in the lock. Today
`[sources]` is an ordinary option, read by the `sources` module while the configuration is
evaluated.

Two things need more than that:

- Shared configuration from elsewhere, such as a company's template for Rust services that every
  project includes as defaults (0007). Files are included while loading, before anything is
  evaluated, so the URL they come from has to be known by then.
- User and project configurations need their own sources and lock. As an option, `sources` sits
  at the top of the system's schema, where a home rooted at `users.<name>` can't reach it.

## Decision

### Sources are part of the frame

`[sources]` is read while loading, like `include` and `[machine]`, from any file of the
configuration. The syntax is the same:

```toml
[sources.acme]
url = "github:acme/distro-templates"
```

- Every kind of configuration has sources the same way: system, user and project.
- Two files defining a source differently is an error naming both, like any other value.
- Errors and `why` point at the line, as they do for options.
- Modules no longer compute sources. Nothing needed to.

### One kind of source

A source is a URL pinned in the lock. It can provide packages, files to include, or both. A
source used only for files has no packages and isn't evaluated as a package set.

### Including a file from a source

```toml
include = [{ source = "acme", path = "rust-service.toml", as = "defaults" }]
```

- The source is declared in a local file of the configuration.
- A file from a source can include other files from the same source, by a path relative to the
  source's root.
- A file from a source can't declare sources or include local files. What a configuration depends
  on is listed in its own files, and the lock stays flat.
- Locations read `acme:rust-service.toml:3`, in errors, `why` and plans.

### Locking and fetching

The lock pins a source used for files the same way it pins one used for packages. The backend
fetches the pinned tree and keeps it in a cache; Nix does this today, and nothing about it shows
in TOML or output.

The rules are the ones packages follow:

- `check`, `why` and `plan` work offline once the source is locked and fetched.
- A source that isn't locked yet is an error at `check` that says to run `distro lock`. `lock`,
  `apply` and `try` lock and fetch what's missing.
- `update acme` moves the pin, and `plan` shows what changed because of it.

### Later

Modules and libraries from sources can use the same mechanism. Not part of this decision.

## Rejected

- Fetching by URL in the include itself (`include = ["https://..."]`). The URL would be pinned
  somewhere other than the lock, and there would be two ways to name where things come from.
- Keeping sources an option and evaluating twice, once to find sources and once with the
  included files. Sources could then depend on what they include.
- Transitive sources, where a file from a source declares its own. Reviewing what a configuration
  depends on would mean reading files from elsewhere.

## Consequences

- The `sources` module goes; `sources` stops being an option, and `options` no longer lists it.
- User and project configurations get sources and a lock without fitting into the system's
  schema.
- A source becomes more than a package set, so `distro sources` says what each provides.
