# Writing modules

A module declares options and turns their values into a system: files, units, packages, users.
Everything you set in TOML is read by some module. `[services.ssh]` is the ssh module's,
`[users.*]` the users module's, and so on.

Modules are written in [Starlark](https://github.com/bazelbuild/starlark), which is basically a
small, deterministic Python: no imports beyond `load`, no I/O, no clocks. Modules also have type
annotations, and wrath checks them when it loads them, so a typo in a module shows up as an
error with a line number instead of a mystery later.

The built-in modules live in [grapes](https://github.com/wrath-os/grapes), in `modules/`. To
bring your own, or to build something quite different, a system builds on a platform of your
own (see Platforms below).

## A small one

Here's a complete module, which writes `/etc/motd`:

```python
# modules/motd.star
# The message of the day, shown when someone logs in on a console or over SSH.

def render(ctx: Context) -> None:
    cfg = ctx.read("motd")
    if not cfg["text"].strip():
        ctx.error("motd.text", "an empty message of the day: turn `motd` off instead")
    text = [cfg["text"]]
    if cfg["show_hostname"]:
        text.insert(0, "Welcome to " + ctx.read("system.hostname"))
    ctx.file("/etc/motd", lines(text))

module(
    name = "motd",
    options = {
        "motd": types.record(
            text = option(types.str, required = True, doc = "What to show."),
            show_hostname = option(types.bool, default = True),
        ),
    },
    writes = [],
    enable = False,
    render = render,
)
```

And using it:

```toml
[motd]
enable = true
text = "Be nice."
```

`enable = False` gives the module a `motd.enable` option that's off by default, and `render`
only runs when it's on. A module that should always run leaves `enable` out.

## Options

`options` maps dotted names to types. Each `option(...)` has a type and optionally a `default`,
`doc`, `required = True` or `optional = True`:

| Type | Holds |
|---|---|
| `types.str`, `types.int`, `types.bool` | what you'd expect |
| `types.path`, `types.port` | a path, a port number |
| `types.range(1, 10)` | an integer in that range |
| `types.one_of(["a", "b"])` | one of those strings |
| `types.list(t)` | a list where one value wins as a whole |
| `types.set(t)` | a list merged from every file that sets it, sorted, without duplicates |
| `types.ordered(t)` | named items placed with `before` and `after` anchors |
| `types.map(t)` | named entries the configuration picks, like `services.nginx.<name>` |
| `types.record(...)` | a table with these fields |
| `types.freeform` | anything, for settings passed straight through to a program |

The type decides how values from several files combine. Two files setting a `str` to different
values is an error, a `set` gets both files' items, and so on. `doc` shows up in
`wrath options`, so write one for anything that isn't obvious from its name.

`user_options` declares options every user has, under `users.<name>`. The ssh module uses it for
`authorized_keys`.

## Rendering

`render(ctx)` reads options and says what the system gets. The things you'll use most:

| Call | Does |
|---|---|
| `ctx.read("services.ssh")` | an option's value, or a dict of everything under it |
| `ctx.file(path, text, mode)` | a file in `/etc`, or with `user =`, in a home |
| `ctx.unit(name, sections, restart_on = [...])` | a systemd unit, restarted when the listed files change |
| `ctx.package("jq")` | a package on the system, or with `user =`, for one user |
| `ctx.set(option, value)` | sets another module's option |
| `ctx.default(option, value)` | the same, but anything in the configuration wins |
| `ctx.error(option, message)` | stops with an error pointing at where the option is set |
| `ctx.wrapper(name, source = ...)` | a setuid copy in `/run/wrath/wrappers/bin` |
| `ctx.secret(name, ...)` | a secret decrypted on the machine |

`formats.json`, `formats.toml` and `formats.systemd` turn a dict into a file's text, and
`lines([...])` joins lines with a newline after each.

Every option a module sets with `ctx.set` has to be listed in its `writes`. That's how wrath
knows to run the ssh module before the firewall module reads `allowed_tcp_ports`, and it means
there's never a question of which module ran first.

Shared code goes in `modules/lib/` and is loaded like so:

```python
load("lib/pam.star", "unix_stacks")
```

## Errors

Use `ctx.error` rather than `fail()` when the problem is in what someone wrote. It points at the
line in their TOML:

```text
error: an empty message of the day: turn `motd` off instead
 --> /etc/wrath/system.toml:6:8
  |
6 | text = "  "
  |        ^^^^ `motd.text` is set here
  = reported by modules/motd.star
```

## Testing

A module's tests are configurations with the files they should produce. Make a directory under
`modules/tests/`, with a `system.toml` and an empty file for every path you want checked:

```text
modules/tests/motd/
├── system.toml
└── expected/etc/motd
```

Run it, look at what it says, and bless it when it's right:

```console
$ wrath dev test modules/tests
FAIL  motd: etc/motd differs
      expected:
      actual:
        Welcome to box
        Be nice.
$ wrath dev test modules/tests --bless
$ wrath dev test modules/tests
PASS  motd
```

Units go under `expected/etc/systemd/system/`, and a user's files under `expected/home/NAME/`.
Bad configurations work the same way, with an `expected-error.txt` instead, and
`wrath dev test fixtures/bad-configs` checks that each still fails with the same error.

## Platforms

Every system builds on a platform, a set of modules. Unless it says otherwise, that's grapes':
the modules a wrath system is made of. You don't need to declare grapes: your lock pins it the
first time it's read, `wrath update grapes` moves it forward, and a rollback takes it back. To
build from a checkout of grapes for one command, give it with `--source grapes=DIR`. A platform
of your own lives in a source, with a `platform.toml` that lists its modules:

```toml
# platform.toml, in the acme source
modules = ["modules"]                       # acme's own, in modules/
from-grapes = ["dbus", "polkit", "users"]  # grapes' modules acme keeps
```

A system chooses it in the file it starts from, `system.toml` or a role's file:

```toml
platform = { source = "acme" }

[sources.acme]
url = "github:acme/platform"
```

A platform of your own changes only the modules: packages still come from grapes' recipes, so
something built your own way still gets the same packages, and ones you add with recipes of your
own.

A module is in the system because the platform lists it, so leaving one of ours out is leaving it
off the list. Two modules declaring the same option is an error, and so is a module writing to an
option nothing on the platform declares, such as `ssh` opening a firewall port on a platform
without `firewall`. The lock pins the platform's source like any other, and the plan shows when a
system moves to another platform.

## Reading more

The built-in modules are the best examples. `ssh.star`, `greetd.star` and `syncthing.star` are
short and cover most of the above. [The module API](reference/module-api.md) lists every call
and [Options](https://github.com/wrath-os/grapes/blob/main/docs/reference/options.md) every
option the built-in modules declare.
