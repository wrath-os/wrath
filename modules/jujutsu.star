# Jujutsu for each user who enables it: `~/.config/jj/config.toml`, with commits signed by an
# SSH key such as a secret.

load("lib/merge.star", "merge")

def render(ctx: Context) -> None:
    for user, u in ctx.read("users").items():
        jj = u["programs"]["jujutsu"]
        if not jj["enable"]:
            continue
        settings = {}
        for key in ["name", "email"]:
            if key in jj:
                settings = merge(settings, {"user": {key: jj[key]}})
        if "signing_key" in jj:
            settings = merge(settings, {
                "signing": {"behavior": "own", "backend": "ssh", "key": jj["signing_key"]},
            })
        settings = merge(settings, jj["settings"])
        ctx.file("~/.config/jj/config.toml", formats.toml(settings), user = user)
        ctx.package("jujutsu", user = user)

module(
    name = "jujutsu",
    options = {},
    user_options = {
        "programs.jujutsu": types.record(
            enable = option(types.bool, default = False),
            name = option(types.str, optional = True),
            email = option(types.str, optional = True),
            signing_key = option(
                types.path,
                optional = True,
                runtime = True,
                doc = "An SSH private key, or a secret, that signs commits.",
            ),
            settings = option(
                types.freeform,
                default = {},
                doc = "Anything else in jj's config, such as `ui.editor`.",
            ),
        ),
    },
    writes = [],
    render = render,
    configurations = ["system", "home"],
)
