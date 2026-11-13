# The Ghostty terminal and its settings, as `key = value` lines in
# `~/.config/ghostty/config.ghostty`, a list repeating its key, as `keybind` does. Ghostty reads no
# system-wide file, so the system's settings are written for every user, with a user's own over
# them. It reads the file again on its `reload_config` action, ctrl+shift+comma by default.

load("lib/merge.star", "merge")

CONFIG = "~/.config/ghostty/config.ghostty"

def line(key: str, value: typing.Any) -> str:
    if type(value) == "bool":
        value = "true" if value else "false"
    return "%s = %s\n" % (key, value)

def config(ctx: Context, at: list[str], settings: dict[str, typing.Any]) -> str:
    """`settings` as Ghostty writes them, each key on a line of its own, or a line per item."""
    text = ""
    for key, value in sorted(settings.items()):
        if type(value) == "dict":
            ctx.error(at + [key], "Ghostty's settings are `key = value`, without tables")
        for item in (value if type(value) == "list" else [value]):
            text += line(key, item)
    return text

def render(ctx: Context) -> None:
    system = ctx.read("programs.ghostty")["settings"]
    if system:
        ctx.package("ghostty")
    for user, u in ctx.read("users").items():
        if u["system"]:
            continue
        own = u["programs"]["ghostty"]["settings"]
        if own:
            ctx.package("ghostty", user = user)
        settings = merge(system, own)
        if settings:
            at = ["users", user, "programs", "ghostty", "settings"]
            ctx.file(CONFIG, config(ctx, at, settings), user = user, setting = True)

SETTINGS = types.record(
    settings = option(
        types.freeform,
        default = {},
        doc = "Ghostty's settings, such as `font-size = 12` or `keybind = [\"ctrl+t=new_tab\"]`. " +
              "The system's are every user's, and a user's own go over them.",
    ),
)

module(
    name = "ghostty",
    options = {"programs.ghostty": SETTINGS},
    user_options = {"programs.ghostty": SETTINGS},
    writes = [],
    render = render,
)
