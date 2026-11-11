# 0027: Settings for any program

Status: accepted, 2026-11-11.

## Context

A module types the options it declares. That's right for what distro reasons about: whether a
service runs, its port, its user, the paths it writes. It doesn't scale to every setting of every
program. Programs add and rename settings with each release, a typed copy of them is always
behind, and every module needs an escape hatch for what it doesn't cover anyway.

NixOS reached the same point and adopted RFC 42: a module takes `settings`, an untyped structure,
and renders it with `formats.toml`, `formats.yaml` and the like into the program's own file.
That works, but each program still needs a module written for it before it can be configured,
and nothing checks the settings until the program reads them.

We already have the parts: `types.freeform`, and `formats.toml`, `formats.json` and
`formats.systemd` for modules. git's module takes `settings` and renders them.

## Decision

Three layers, each used only when the one before doesn't do.

A recipe says where its program reads its settings, and in what format:

```toml
[[package]]
id = "alacritty"
config.settings = { file = "alacritty/alacritty.toml", format = "toml" }
```

`file` is under the configuration directory: `/etc` for a system, `~/.config` for a user. Then
any packaged program takes settings with no module written for it:

```toml
[programs.alacritty.settings]
font.size = 12
window.opacity = 0.95
```

A program with more than one file names each, and each is an option of its own. Helix reads
its editor settings from `config.toml` and its languages from `languages.toml`:

```toml
[[package]]
id = "helix"
config.settings = { file = "helix/config.toml", format = "toml" }
config.languages = { file = "helix/languages.toml", format = "toml" }
```

```toml
[programs.helix.settings]
theme = "onedark"

[[programs.helix.languages.language]]
name = "rust"
auto-format = true
```

- `programs.<id>.<name>` exists for each file a package's recipe names under `config`, for the
  system and for each user, with `settings` the usual name for a program's main file. Setting
  any of them adds the package, and writes that file in its format.
- When the package ships a JSON Schema for a file, named with `schema` beside its `file`, plan
  checks the settings against it, and an error points at the line in the configuration. Types
  come from the program's authors, and stay as current as the package.
- A module for a program keeps a small typed core, only what distro reasons about or other modules
  read, and takes the rest as `settings` in the same shape. A module and a recipe's `config` for
  the same program don't both write the file: the module owns it, and takes the settings.
- What `settings` can't express goes in `files`, as text. What needs more than a file, such as a
  unit, a PAM entry or a firewall port, is a module.

The formats are written in Rust, as `formats.*` are now: TOML, JSON, YAML, INI, KDL (niri's),
`key = value` and systemd's. A format is added when a recipe's `config` needs it.

## Consequences

- Many programs can be configured as soon as they have a recipe and one line of `config` in it,
  with no module. A module is written only for integration.
- Settings render to the same file whichever layer wrote them, so moving from recipe settings to a
  module's doesn't change the file.
- Untyped settings fail when the program reads them, unless the program ships a schema. Plan says
  which programs' settings it checked.
- `programs.<id>` is shared by modules and recipes. A module that declares `programs.<id>` takes
  it over, and the recipe's `config` then only says where the file goes.
- Replacing a built-in module with one of the configuration's own, which 0026 left open, is the
  next question: it is what makes NixOS modules attractive, and it fits here as a configuration's
  module declaring the same options, with plan naming what it replaced.
