# Firefox's policies and settings: see `lib/gecko.star` for where each goes. Its profiles are in
# `~/.config/mozilla/firefox`, unless `~/.mozilla/firefox` exists from before, which it then
# reads instead.

load("lib/gecko.star", "options", "render_browser", "user_options")

def render(ctx: Context) -> None:
    render_browser(ctx, "firefox", "firefox", "~/.config/mozilla/firefox")

module(
    name = "firefox",
    options = {"programs.firefox": options("Firefox")},
    user_options = {"programs.firefox": user_options("Firefox")},
    writes = [],
    render = render,
)
