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

One table, `modules/lib/programs.star`, says for each program the files it reads and their
format. It's beside the modules, not in the recipes: a configuration is evaluated with the
modules alone, on a machine as in a checkout, and the recipes stay in the repository.

```python
SETTINGS = {
    "alacritty": {
        "settings": {
            "file": "alacritty/alacritty.toml",
            "system": "/etc/xdg/alacritty/alacritty.toml",
            "format": "toml",
        },
    },
}
```

`file` is where a user's goes, under `~/.config`. `system` is where the system-wide one goes, for
a program that reads one. Then the program takes settings with no module written for it:

```toml
[programs.alacritty.settings]
font.size = 12
window.opacity = 0.95
```

A program with more than one file names each, and each is an option of its own. Helix reads
its editor settings from `config.toml` and its languages from `languages.toml`:

```python
"helix": {
    "settings": {"file": "helix/config.toml", "format": "toml"},
    "languages": {"file": "helix/languages.toml", "format": "toml"},
},
```

```toml
[programs.helix.settings]
theme = "onedark"

[[programs.helix.languages.language]]
name = "rust"
auto-format = true
```

- `users.<user>.programs.<id>.<name>` exists for each file the table names, and
  `programs.<id>.<name>` for those with a `system` place, with `settings` the usual name for a
  program's main file. Setting any of them adds the package, for the system or that user, and
  writes the file in its format. Settings for a program with no system-wide file are an unknown
  option in a system configuration.
- When the package ships a JSON Schema for a file, named with `schema` beside its `file`, plan
  checks the settings against it, and an error points at the line in the configuration. Types
  come from the program's authors, and stay as current as the package. Not built yet.
- A module for a program keeps a small typed core, only what distro reasons about or other modules
  read, and takes the rest as `settings` in the same shape. A program with a module isn't in the
  table: the module owns its files.
- What `settings` can't express goes in `files`, as text. What needs more than a file, such as a
  unit, a PAM entry or a firewall port, is a module.

The formats are written in Rust, as `formats.*` are: TOML and JSON now, and YAML, INI, KDL
(niri's) and `key = value` as programs in the table need them.

## Consequences

- Many programs can be configured as soon as they have a recipe and a line in the table, with no
  module. A module is written only for integration.
- Settings render to the same file whichever layer wrote them, so moving a program from the table
  to a module of its own doesn't change the file.
- Untyped settings fail when the program reads them, unless the program ships a schema. Plan says
  which programs' settings it checked.
- `programs.<id>` is shared by the table and modules. Both declaring the same program is an
  error when the modules load, so a program moving to a module leaves the table.
- Replacing a built-in module with one of the configuration's own, which 0026 left open, is the
  next question: it is what makes NixOS modules attractive, and it fits here as a configuration's
  module declaring the same options, with plan naming what it replaced.
