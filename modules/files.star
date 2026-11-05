# Files declared directly in the configuration, for anything no module renders.

file = types.record(
    text = option(types.str),
    mode = option(types.int, default = 0o644),
)

def render(ctx):
    for path, f in ctx.read("files").items():
        if not path.startswith("/"):
            fail("files: `%s` is not an absolute path" % path)
        ctx.file(path, f["text"], f["mode"])

module(
    name = "files",
    options = {"files": types.map(file)},
    writes = [],
    render = render,
)
