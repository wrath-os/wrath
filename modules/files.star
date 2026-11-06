# Files declared directly in the configuration, for anything no module renders: system files
# under `files`, and files in a user's home under `files` in `users/<name>.toml`.

file = types.record(
    text = option(types.str),
    mode = option(types.int, default = 0o644),
)

def render(ctx):
    for path, f in ctx.read("files").items():
        if not path.startswith("/"):
            fail("files: `%s` is not an absolute path" % path)
        ctx.file(path, f["text"], f["mode"])
    for user, u in ctx.read("users").items():
        for path, f in u["files"].items():
            if not path.startswith("~/"):
                fail("users.%s.files: `%s` doesn't start with `~/`" % (user, path))
            ctx.file(path, f["text"], f["mode"], user = user)

module(
    name = "files",
    options = {"files": types.map(file)},
    user_options = {"files": types.map(file)},
    writes = [],
    render = render,
)
