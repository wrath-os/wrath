<!-- Made by `wrath dev docs` from the code. Don't edit it by hand. -->

# The module API

## Functions

### `lines(items)`

The lines of `items`, each ending in a newline, as a file holds them.

### `machine_value(name)`

A machine value, such as `machine_value("machine.name")`, filled in on the machine when it switches. Only options and files filled in there take it, like a secret.

### `module(*, name, options, user_options = ..., writes, render, configurations = ..., enable = ...)`

Declares the module. Every module file calls this once. `user_options` are options every user has, under `users.<name>`.

### `option(type, *, default = ..., optional = False, required = False, runtime = False, account = False, doc = "")`

Declares an option: a type with a default, documentation and flags. A list or table is empty when nothing sets it, unless it's `optional` or `required`.

## `formats`

### `formats.json(value)`

Pretty-printed JSON with sorted keys.

### `formats.kdl(value)`

A KDL document, as niri reads, from a dictionary of nodes. Each key is written as it is, arguments and properties included, such as `output "eDP-1"`. A string, number or list of them gives the node's arguments, `True` makes it a flag and `False` leaves it out, a dictionary gives its children, and a list of dictionaries or lists repeats it.

### `formats.systemd(sections)`

A systemd-style file, such as a `.network` file, from a dictionary of sections. A list value repeats its key.

### `formats.toml(value)`

A TOML document from a dictionary, with keys sorted.

## `types`

Types that take nothing: `types.bool`, `types.int`, `types.float`, `types.str`, `types.path`, `types.file`, `types.port`, `types.freeform`.

### `types.one_of(choices)`

One of these strings.

### `types.range(min, max)`

An integer from `min` to `max`.

### `types.ordered(item)`

A list of named items placed with `before` and `after` anchors.

### `types.set(item)`

A list merged from every definition, sorted and without duplicates.

### `types.record(**fields)`

A table with these fields.

### `types.list(item)`

A list where one value wins as a whole.

### `types.map(item)`

Named entries chosen by the configuration.

## `ctx`

What a module's `render` is given, to read options and say what the system gets.

### `ctx.base(key, value)`

Sets one top-level setting of the base system the backend provides, such as `audio`.

### `ctx.build(name, *, package = None, options = ..., patches = ...)`

Makes `name` a package built from `package`, by default the one called `name`, with other values for its build's `options` and the `patches` applied, paths in the configuration.

### `ctx.default(path, value)`

Sets an option at the default level, so configuration overrides it.

### `ctx.enable(unit, *, user = False, wanted_by = ...)`

Enables a unit a service package ships, as `systemctl enable` does: by its [Install] section, or wanted by the targets in `wanted_by` instead. With `user`, a user unit, for every user.

### `ctx.error(path, message)`

Stops the render with `message`, pointing at where the configuration sets the option at `path`, or the first option under it, so the error shows what to fix. `path` is dotted, or a list of keys for keys with dots in them. Without such a place, the error points at the module.

### `ctx.file(path, text, mode = None, *, user = ..., owner = None, group = None, next_login = False, setting = False)`

Renders a file. With `user`, the path starts with `~/` and the file goes in that user's home. Text with secrets in it, a runtime value, makes a file the machine fills in, owned by `owner` and `group` with `mode` (0400 by default); other files are links into the store that anyone can read. `next_login` says the file is only read when a user logs in, so changing it takes effect at the next login. `setting` says it's a program's own settings, which a bad edit can't lock anyone out with, so watching applies a change to it without asking.

### `ctx.locked_ids()`

The uids and gids the lock keeps, as `{"users": {name: uid}, "groups": {name: gid}}`. A module that hands out new ones records them all with `ctx.base("ids", ...)`, and the lock keeps them from then on.

### `ctx.package(name, *, user = ...)`

Adds a package to the system, or with `user`, to that user's packages. `stable#git` is `git` from the source `stable`.

### `ctx.read(path)`

The value of an option, or a dictionary of everything under a table of options.

### `ctx.reload(unit, *, on)`

Reloads a service when one of the files in `on` changes: one the system provides, such as `systemd-networkd.service`, or one a module renders that can take a new configuration without stopping.

### `ctx.remove(path, items)`

Removes items from a set or ordered list.

### `ctx.restart(unit, *, on)`

Restarts a service the system provides when one of the files in `on` changes.

### `ctx.secret(name, *, file, owner = "root", group = None, mode = ..., path = None)`

Declares a secret, kept encrypted in `file` and decrypted on the machine to `/run/wrath/secrets/<name>`, owned by `owner` and `group` with `mode`, and linked from `path`.

### `ctx.service_package(name)`

Adds a package whose units, users, tmpfiles, udev rules and D-Bus services join the system, such as `dbus`. Its programs are added too.

### `ctx.set(path, value)`

Sets an option.

### `ctx.unit(name, sections, *, restart_on = ..., user = ...)`

Renders a systemd unit from a dictionary of sections. `restart_on` lists files the unit reads, so it restarts when one of them changes. With `user`, the unit runs in that user's systemd user manager.

### `ctx.wrapper(name, *, source, owner = ..., group = ..., mode = ...)`

Puts a copy of `source` in `/run/wrath/wrappers/bin/<name>` with its own owner and mode, such as setuid root, for programs that run with more privilege than their caller.
