# The niri session: the compositor, run as a user service; the desktop portals screen sharing
# and file pickers go through, wlroots' for the screen and GTK's for the rest; and GNOME Keyring
# for secrets.

PORTALS = """[preferred]
default=gtk
org.freedesktop.impl.portal.Access=gtk
org.freedesktop.impl.portal.FileChooser=gtk
org.freedesktop.impl.portal.Notification=gtk
org.freedesktop.impl.portal.ScreenCast=wlr
org.freedesktop.impl.portal.Screenshot=wlr
org.freedesktop.impl.portal.Secret=gnome-keyring
"""

# niri reads a user's own config, or else this one. Either way it watches the file and what it
# includes, and applies a change as soon as it's saved.
SYSTEM_CONFIG = "/etc/niri/config.kdl"
USER_CONFIG = "~/.config/niri/config.kdl"
DEFAULTS = "/run/system/share/niri/default-config.kdl"

# Nodes whose settings are properties, written on the node as `match app-id="firefox"`, where a
# table would otherwise be the node's children.
PROPERTY_NODES = [
    "match", "exclude", "focus-follows-mouse", "warp-mouse-to-focus", "offset", "position",
    "default-floating-position", "spring", "length", "scroll-factor", "variable-refresh-rate",
    "gradient", "active-gradient", "inactive-gradient", "urgent-gradient",
]

# What a bind says about itself, as properties on its keys, beside the action it runs.
BIND_PROPERTIES = ["allow-when-locked", "allow-inhibiting", "cooldown-ms", "repeat",
                   "hotkey-overlay-title"]

# Rules, where `true` and `false` are given, as in `open-floating true`, except for the `on` and
# `off` flags; elsewhere `true` is a flag such as `prefer-no-csd`.
RULES = ["window-rule", "layer-rule"]
RULE_FLAGS = ["on", "off"]

def with_properties(name: str, properties: dict[str, typing.Any]) -> str:
    """`name` with `properties` written on it, as a node's head."""
    return name + "".join([
        " %s=%s" % (key, json.encode(value))
        for key, value in sorted(properties.items())
    ])

def shaped(settings: dict[str, typing.Any], in_rule: bool = False) -> dict[str, typing.Any]:
    """`settings` as `formats.kdl` writes them, with what niri takes as properties and given
    booleans moved into the nodes' heads."""
    nodes = {}
    for name, value in settings.items():
        if name in PROPERTY_NODES and type(value) in ["dict", "list"]:
            for properties in (value if type(value) == "list" else [value]):
                nodes[with_properties(name, properties)] = True
        elif name == "binds" and type(value) == "dict":
            nodes[name] = {
                with_properties(keys, {k: v for k, v in bind.items() if k in BIND_PROPERTIES}): {
                    k: v
                    for k, v in bind.items()
                    if k not in BIND_PROPERTIES
                }
                for keys, bind in value.items()
            }
        elif type(value) == "dict":
            nodes[name] = shaped(value, in_rule)
        elif type(value) == "list" and name in RULES:
            nodes[name] = [shaped(rule, True) for rule in value]
        elif type(value) == "bool" and in_rule and name not in RULE_FLAGS:
            nodes[name] = [value]
        else:
            nodes[name] = value
    return nodes

def config(base: str, settings: dict[str, typing.Any]) -> str:
    """`settings` over the config at `base`, which niri reads first. niri allows a section once
    in a file, and merges across includes: a later setting wins over an earlier one, and a bind
    replaces the one for the same keys."""
    return 'include "%s"\n' % base + formats.kdl(shaped(settings))

def render(ctx: Context) -> None:
    desktop = ctx.read("desktop")
    system = ctx.read("programs.niri")["settings"]
    users = {
        user: u["programs"]["niri"]["settings"]
        for user, u in ctx.read("users").items()
    }
    if desktop.get("session") != "niri":
        if system:
            ctx.error("programs.niri.settings", "niri is the desktop only with session = \"niri\"")
        return
    for package in [
        "niri",
        "xdg-desktop-portal",
        "xdg-desktop-portal-gtk",
        "xdg-desktop-portal-wlr",
        "gnome-keyring",
        # The accessibility bus GTK programs ask for, started by D-Bus.
        "at-spi2-core",
    ]:
        ctx.service_package(package)
    # niri runs X programs through it, and it runs Xwayland.
    ctx.package("xwayland-satellite")
    ctx.package("xwayland")
    # The Compose tables dead keys and compose sequences come from, which xkbcommon reads from
    # libX11's share/X11/locale.
    ctx.package("libX11")
    ctx.file("/etc/xdg/xdg-desktop-portal/niri-portals.conf", PORTALS)
    ctx.file(SYSTEM_CONFIG, config(DEFAULTS, system), setting = True)
    for user, own in users.items():
        if own:
            ctx.file(USER_CONFIG, config(SYSTEM_CONFIG, own), user = user, setting = True)

SETTINGS = types.record(
    settings = option(
        types.freeform,
        default = {},
        doc = "niri's settings, over its defaults, written as KDL: a table is a section and " +
              "`true` a flag such as `prefer-no-csd`. `match = { app-id = \"firefox\" }` and " +
              "a bind's `allow-when-locked` are properties, and in a rule `true` is given, as " +
              "`open-floating true`. A key can carry arguments, such as `'output \"eDP-1\"'`, " +
              "and a list of tables or lists repeats a node, such as `spawn-at-startup`.",
    ),
)

module(
    name = "niri",
    options = {"programs.niri": SETTINGS},
    user_options = {"programs.niri": SETTINGS},
    writes = [],
    render = render,
)
