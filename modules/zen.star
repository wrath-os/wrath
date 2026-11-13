# Zen's policies and settings: see `lib/gecko.star` for where each goes. Its profiles are in
# `~/.config/zen`, unless `~/.zen` exists from before, which it then reads instead.

load("lib/gecko.star", "options", "render_browser", "user_options")

def render(ctx: Context) -> None:
    render_browser(ctx, "zen", "zen-browser", "~/.config/zen")

module(
    name = "zen",
    options = {"programs.zen": options("Zen")},
    user_options = {"programs.zen": user_options("Zen")},
    writes = [],
    render = render,
)
