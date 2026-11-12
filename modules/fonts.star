# Fonts for a desktop: a face for text, one for code with icons, and emoji, which programs find
# through fontconfig by the generic names `sans-serif`, `serif`, `monospace` and `emoji`.

PRESETS = "/run/system/share/fontconfig/conf.avail/"

# fontconfig's presets that come before the system's choices: aliases between metric-compatible
# faces, and the guesses that give a request for "Sans" or "DejaVu Sans Mono" its generic name.
BEFORE = [
    "30-metric-aliases.conf",
    "40-nonlatin.conf",
    "45-generic.conf",
    "45-latin.conf",
    "48-guessfamily.conf",
    "48-spacing.conf",
    "49-sansserif.conf",
]

# And those after: each user's own settings, then the faces each generic name falls back to.
AFTER = [
    "50-user.conf",
    "60-generic.conf",
    "60-latin.conf",
    "65-nonlatin.conf",
    "70-no-bitmaps-except-emoji.conf",
    "90-synthetic.conf",
]

# Smooth edges and slight hinting, without colour fringes, which suits any screen. A user's own
# settings come before this, so for these they go in a match of their own on `target="font"`.
RENDERING = """  <match target="font">
    <edit name="antialias" mode="assign"><bool>true</bool></edit>
    <edit name="hinting" mode="assign"><bool>true</bool></edit>
    <edit name="hintstyle" mode="assign"><const>hintslight</const></edit>
    <edit name="rgba" mode="assign"><const>none</const></edit>
    <edit name="lcdfilter" mode="assign"><const>lcddefault</const></edit>
  </match>
"""

FONTS = ["dejavu-fonts", "inter", "nerd-fonts-jetbrains-mono", "noto-color-emoji"]

def includes(presets: list[str]) -> list[str]:
    return ['  <include ignore_missing="yes">%s%s</include>' % (PRESETS, p) for p in presets]

def fonts_conf(families: dict[str, str]) -> str:
    lines = [
        '<?xml version="1.0"?>',
        '<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">',
        "<fontconfig>",
        "  <dir>/run/system/share/fonts</dir>",
        '  <cachedir prefix="xdg">fontconfig</cachedir>',
    ]
    lines += includes(BEFORE)
    for generic in ["sans-serif", "serif", "monospace", "emoji"]:
        prefer = [families[generic]]
        if generic != "emoji":
            prefer.append(families["emoji"])
        lines += ['  <alias binding="same">', "    <family>%s</family>" % generic, "    <prefer>"]
        lines += ["      <family>%s</family>" % family for family in prefer]
        lines += ["    </prefer>", "  </alias>"]
    lines += includes(AFTER)
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
