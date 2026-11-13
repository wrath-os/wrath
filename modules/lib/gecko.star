# Settings for browsers built on Firefox, which modules load with
# `load("lib/gecko.star", "options", "render_browser", "user_options")`.
#
# Policies, which only a system sets, go in `/etc/<name>/policies/policies.json`, which the
# browser reads instead of the one it ships with, so the system's file also keeps it from
# updating itself. Settings, the preferences `about:config` lists, go in `user.js` in a profile
# wrath declares in the browser's `profiles.ini`, which the browser applies each time it starts.
# The browser keeps its own profile list unless it starts with `MOZ_LEGACY_PROFILES=1`, as
# wrath's packages of it do.

load("lib/merge.star", "merge")

PROFILES = """[General]
StartWithLastProfile=1
Version=2

[Profile0]
Name=default
IsRelative=1
Path=default
Default=1
"""

def preferences(
        ctx: Context,
        at: list[str],
        settings: dict[str, typing.Any],
        prefix: str = "") -> dict[str, typing.Any]:
    """`settings` with nested tables joined into dotted names, as TOML's dotted keys make
    `browser.startup.homepage` a table in a table."""
    out = {}
    for key, value in settings.items():
        if type(value) == "dict":
            out.update(preferences(ctx, at + [key], value, prefix + key + "."))
        elif type(value) == "list":
            ctx.error(at + [key], "a preference is a string, a number or true or false")
        else:
            out[prefix + key] = value
    return out

def quoted(text: str) -> str:
    return '"%s"' % text.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")

def value_of(value: typing.Any) -> str:
    if type(value) == "bool":
        return "true" if value else "false"
    if type(value) == "string":
        return quoted(value)
    return str(value)

def user_js(prefs: dict[str, typing.Any]) -> str:
    return "".join([
        "user_pref(%s, %s);\n" % (quoted(name), value_of(value))
        for name, value in sorted(prefs.items())
    ])

def render_browser(ctx: Context, name: str, package: str, profiles: str) -> None:
    """Writes the policies and settings of `programs.<name>`, for a browser whose profiles are
    under `profiles` in each home."""
    system = ctx.read("programs." + name)
    if system["policies"]:
        ctx.package(package)
        policies = merge({"DisableAppUpdate": True}, system["policies"])
        ctx.file("/etc/%s/policies/policies.json" % name, formats.json({"policies": policies}))
    if system["settings"]:
        ctx.package(package)
    for user, u in ctx.read("users").items():
        if u["system"]:
            continue
        own = u["programs"][name]["settings"]
        if own:
            ctx.package(package, user = user)
        settings = merge(system["settings"], own)
        if not settings:
            continue
        at = ["users", user, "programs", name, "settings"]
        ctx.file(profiles + "/profiles.ini", PROFILES, user = user, setting = True)
        prefs = preferences(ctx, at, settings)
        ctx.file(profiles + "/default/user.js", user_js(prefs), user = user, setting = True)

def settings_option(browser: str) -> typing.Any:
    return option(
        types.freeform,
        default = {},
        doc = "%s's preferences, as `about:config` names them, such as " % browser +
              "`\"browser.startup.homepage\" = \"https://example.org\"`, applied each time it " +
              "starts. The system's are every user's, and a user's own go over them.",
    )

def options(browser: str) -> typing.Any:
    return types.record(
        policies = option(
            types.freeform,
            default = {},
            doc = "%s's enterprise policies, such as `DisableTelemetry = true` " % browser +
                  "or `ExtensionSettings`, for every user. Updating itself stays off.",
        ),
        settings = settings_option(browser),
    )

def user_options(browser: str) -> typing.Any:
    return types.record(settings = settings_option(browser))
