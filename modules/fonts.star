# Fonts for a desktop: a face for text, one for code with icons, and emoji, which programs find
# through fontconfig by the generic names `sans-serif`, `serif`, `monospace` and `emoji`.

# Smooth edges and slight hinting, without colour fringes, which suits any screen; and each
# user's own settings in ~/.config/fontconfig, which come after and win.
RENDERING = """  <match target="font">
    <edit name="antialias" mode="assign"><bool>true</bool></edit>
    <edit name="hinting" mode="assign"><bool>true</bool></edit>
    <edit name="hintstyle" mode="assign"><const>hintslight</const></edit>
    <edit name="rgba" mode="assign"><const>none</const></edit>
    <edit name="lcdfilter" mode="assign"><const>lcddefault</const></edit>
  </match>
  <include ignore_missing="yes" prefix="xdg">fontconfig/conf.d</include>
  <include ignore_missing="yes" prefix="xdg">fontconfig/fonts.conf</include>
"""

FONTS = ["dejavu-fonts", "inter", "nerd-fonts-jetbrains-mono", "noto-color-emoji"]

def fonts_conf(families: dict[str, str]) -> str:
    lines = [
        '<?xml version="1.0"?>',
        '<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">',
        "<fontconfig>",
        "  <dir>/run/system/share/fonts</dir>",
        '  <cachedir prefix="xdg">fontconfig</cachedir>',
    ]
    for generic in ["sans-serif", "serif", "monospace", "emoji"]:
        prefer = [families[generic]]
        if generic != "emoji":
            prefer.append(families["emoji"])
        lines += ['  <alias binding="same">', "    <family>%s</family>" % generic, "    <prefer>"]
        lines += ["      <family>%s</family>" % family for family in prefer]
        lines += ["    </prefer>", "  </alias>"]
    return "\n".join(lines) + "\n" + RENDERING + "</fontconfig>\n"

def render(ctx: Context) -> None:
    if "session" not in ctx.read("desktop"):
        return
    ctx.package("fontconfig")
    for font in FONTS:
        ctx.package(font)
    families = {
        "sans-serif": ctx.read("fonts.sans"),
        "serif": ctx.read("fonts.serif"),
        "monospace": ctx.read("fonts.monospace"),
        "emoji": ctx.read("fonts.emoji"),
    }
    ctx.file("/etc/fonts/fonts.conf", fonts_conf(families))

def family(default: str, what: str) -> typing.Any:
    return option(
        types.str,
        default = default,
        doc = "The face programs get when they ask for %s. Its package goes in `packages`." % what,
    )

module(
    name = "fonts",
    options = {
        "fonts.sans": family("Inter", "`sans-serif`, which most text is"),
        "fonts.serif": family("DejaVu Serif", "`serif`"),
        "fonts.monospace": family("JetBrainsMono Nerd Font", "`monospace`, for terminals and code"),
        "fonts.emoji": family("Noto Color Emoji", "`emoji`, and for emoji in any text"),
    },
    writes = [],
    render = render,
)
