# Files declared directly in the configuration, for anything no module renders: system files
# under `files`, and files in a user's home under `files` in `users/<name>.toml`. A file is its
# `text`, or `settings` written in a `format`, so a program's configuration can be written as
# TOML. A system file with a secret in it, as `${secret.api-token}`, is filled in on the machine
# and readable only by its owner.

FORMATS = {"json": formats.json, "systemd": formats.systemd, "toml": formats.toml}

fields = {
    "text": option(types.str, optional = True, runtime = True),
    "format": option(
        types.one_of(sorted(FORMATS.keys())),
        optional = True,
        doc = "What to write `settings` as: `systemd` is `[Section]` and `key=value` lines.",
    ),
    "settings": option(
        types.freeform,
        optional = True,
        runtime = True,
        doc = "The file's contents as TOML, written out in `format`.",
    ),
    "mode": option(types.int, optional = True, doc = "0644, or 0400 with secrets in the text."),
    "owner": option(types.str, optional = True, doc = "Only for text with secrets; root."),
    "group": option(types.str, optional = True),
}

file = types.record(
    restart = option(
        types.set(types.str),
        doc = "Services that read this file, restarted when it changes.",
    ),
    **fields
)

def contents(ctx, at, f):
    """What the file described by `f`, at the option path `at`, holds."""
    if "text" in f and "settings" in f:
        ctx.error(at, "a file has `text` or `settings`, not both")
    if "text" in f:
        return f["text"]
    if "settings" not in f:
        ctx.error(at, "a file needs `text`, or `settings` and a `format`")
    if "format" not in f:
        ctx.error(at + ["settings"], "`settings` need a `format` to be written in")
    return FORMATS[f["format"]](f["settings"])

def render(ctx):
    for path, f in ctx.read("files").items():
        at = ["files", path]
        if not path.startswith("/"):
            ctx.error(at, "`%s` is not an absolute path" % path)
        ctx.file(
            path,
            contents(ctx, at, f),
            f.get("mode"),
            owner = f.get("owner"),
            group = f.get("group"),
        )
        for service in sorted(f["restart"]):
            ctx.restart(service, on = [path])
    for user, u in ctx.read("users").items():
        for path, f in u["files"].items():
            at = ["users", user, "files", path]
            if not path.startswith("~/"):
                ctx.error(at, "`%s` doesn't start with `~/`" % path)
            ctx.file(path, contents(ctx, at, f), f.get("mode"), user = user)

module(
    name = "files",
    options = {"files": types.map(file)},
    user_options = {"files": types.map(types.record(**fields))},
    writes = [],
    render = render,
    configurations = ["system", "home"],
)
