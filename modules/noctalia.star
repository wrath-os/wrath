# The noctalia shell around the compositor: its bar, launcher, notifications and lock screen, run
# as a user service, and its settings. noctalia reads every `*.toml` in `~/.config/noctalia`, then
# what its own settings window saved in `~/.local/state/noctalia/settings.toml`, which wins.

load("lib/merge.star", "merge")

CONFIG = "~/.config/noctalia/config.toml"

def render(ctx: Context) -> None:
    system = ctx.read("programs.noctalia")["settings"]
    users = {
        user: u["programs"]["noctalia"]["settings"]
        for user, u in ctx.read("users").items()
        if not u["system"]
    }
    if ctx.read("desktop").get("shell") != "noctalia":
        if system:
            ctx.error(
                "programs.noctalia.settings",
                "noctalia is the shell only with `desktop.shell = \"noctalia\"`",
            )
        return
    ctx.service_package("noctalia")
    # It starts with the graphical session the compositor brings up.
    ctx.enable("noctalia.service", user = True)
    for user, own in users.items():
        settings = merge(system, own)
        if settings:
            ctx.file(CONFIG, formats.toml(settings), user = user, setting = True)

SETTINGS = types.record(
    settings = option(
        types.freeform,
        default = {},
        doc = "noctalia's settings, as in its `config.toml`, such as " +
              "`bar.main.position = \"top\"`. The system's are every user's, and a user's own go " +
              "over them.",
    ),
)

module(
    name = "noctalia",
    options = {"programs.noctalia": SETTINGS},
    user_options = {"programs.noctalia": SETTINGS},
    writes = [],
    render = render,
)
