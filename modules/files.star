# Files declared directly in the configuration, for anything no module renders: system files
# under `files`, and files in a user's home under `files` in `users/<name>.toml`. A system file
# whose text refers to a secret, as `${secret.api-token}`, is filled in on the machine and
# readable only by its owner.

file = types.record(
    text = option(types.str, runtime = True),
    mode = option(types.int, optional = True, doc = "0644, or 0400 with secrets in the text."),
    owner = option(types.str, optional = True, doc = "Only for text with secrets; root."),
    group = option(types.str, optional = True),
)

def render(ctx):
    for path, f in ctx.read("files").items():
        if not path.startswith("/"):
            ctx.error(["files", path], "`%s` is not an absolute path" % path)
        ctx.file(
            path,
            f["text"],
            f.get("mode"),
            owner = f.get("owner"),
            group = f.get("group"),
        )
    for user, u in ctx.read("users").items():
        for path, f in u["files"].items():
            if not path.startswith("~/"):
                ctx.error(["users", user, "files", path], "`%s` doesn't start with `~/`" % path)
            ctx.file(path, f["text"], f.get("mode"), user = user)

module(
    name = "files",
    options = {"files": types.map(file)},
    user_options = {"files": types.map(file)},
    writes = [],
    render = render,
)
