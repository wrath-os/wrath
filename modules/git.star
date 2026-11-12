# Git for each user who enables it: `~/.config/git/config`, with commits signed by an SSH key
# such as a secret, settings that apply only in some directories, LFS and delta.

load("lib/merge.star", "merge")

def value(v: typing.Any) -> str:
    """A value as git reads it: quoted when it has what git would otherwise cut it at or strip, a
    comment's `;` or `#`, a quote, a backslash, a line break or space at either end."""
    if type(v) == "bool":
        return "true" if v else "false"
    text = str(v)
    if text == text.strip() and not any([c in text for c in [";", "#", "\"", "\\", "\n"]]):
        return text
    escaped = text.replace("\\", "\\\\").replace("\"", "\\\"").replace("\n", "\\n")
    return "\"%s\"" % escaped

def section_lines(name: str, keys: dict[str, typing.Any]) -> list[str]:
    """`[name]` and its keys. A list repeats its key."""
    out = ["[%s]" % name]
    for key, v in sorted(keys.items()):
        for item in (v if type(v) == "list" else [v]):
            out.append("\t%s = %s" % (key, value(item)))
    return out

def gitconfig(settings: dict[str, typing.Any]) -> str:
    """Settings as `section.key` or `section.subsection.key`, the way `git config` names them."""
    out = []

    # Git takes the last value of a key, so includes come last to override what's above them.
    order = lambda item: (item[0] in ["include", "includeIf"], item[0])
    for section, keys in sorted(settings.items(), key = order):
        plain = {k: v for k, v in keys.items() if type(v) != "dict"}
        if plain:
            out += section_lines(section, plain)
        for subsection, sub in sorted(keys.items()):
            if type(sub) == "dict":
                out += section_lines('%s "%s"' % (section, subsection), sub)
    return lines(out)

include = types.record(
    condition = option(types.str, doc = "As in `includeIf`, such as `gitdir:~/work/`."),
    settings = option(types.freeform, default = {}),
)

def render(ctx: Context) -> None:
    for user, u in ctx.read("users").items():
        git = u["programs"]["git"]
        if not git["enable"]:
            continue
        settings = {}
        if "name" in git:
            settings = merge(settings, {"user": {"name": git["name"]}})
        if "email" in git:
            settings = merge(settings, {"user": {"email": git["email"]}})
        if "signing_key" in git:
            settings = merge(settings, {
                "user": {"signingKey": git["signing_key"]},
                "gpg": {"format": "ssh"},
                "commit": {"gpgSign": True},
                "tag": {"gpgSign": True},
            })
        if git["lfs"]:
            settings = merge(settings, {"filter": {"lfs": {
                "clean": "git-lfs clean -- %f",
                "process": "git-lfs filter-process",
                "required": True,
                "smudge": "git-lfs smudge -- %f",
            }}})
            ctx.package("git-lfs", user = user)
        if git["delta"]:
            settings = merge(settings, {
                "core": {"pager": "delta"},
                "interactive": {"diffFilter": "delta --color-only"},
            })
            ctx.package("delta", user = user)
        settings = merge(settings, git["settings"])
        for name, entries in [("ignore", git["ignores"]), ("attributes", git["attributes"])]:
            if entries:
                ctx.file("~/.config/git/" + name, lines(entries), user = user)
        for i, inc in enumerate(git["includes"]):
            path = "~/.config/git/include-%d" % (i + 1)
            ctx.file(path, gitconfig(inc["settings"]), user = user)
            settings = merge(settings, {
                "includeIf": {inc["condition"]: {"path": path}},
            })
        ctx.file("~/.config/git/config", gitconfig(settings), user = user)
        ctx.package("git", user = user)

module(
    name = "git",
    options = {},
    user_options = {
        "programs.git": types.record(
            enable = option(types.bool, default = False),
            name = option(types.str, optional = True),
            email = option(types.str, optional = True),
            signing_key = option(
                types.path,
                optional = True,
                runtime = True,
                doc = "An SSH private key, or a secret, that signs commits and tags.",
            ),
            settings = option(
                types.freeform,
                default = {},
                doc = "Anything else, as `section.key` or `section.subsection.key`.",
            ),
            includes = option(types.list(include)),
            lfs = option(types.bool, default = False, doc = "Large files with Git LFS."),
            delta = option(
                types.bool,
                default = False,
                doc = "Show diffs with delta. Its own options go in `settings.delta`.",
            ),
            ignores = option(
                types.list(types.str),
                doc = "Patterns every repository ignores.",
            ),
            attributes = option(
                types.list(types.str),
                doc = "Attributes for every repository, such as `* merge=mergiraf`.",
            ),
        ),
    },
    writes = [],
    render = render,
    configurations = ["system", "home"],
)
