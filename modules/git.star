# Git for each user who enables it: `~/.config/git/config`, with commits signed by an SSH key
# such as a secret, settings that apply only in some directories, LFS and delta.

load("lib/merge.star", "merge")

def value(v):
    if type(v) == "bool":
        return "true" if v else "false"
    return str(v)

def section_lines(name, keys):
    """`[name]` and its keys. A list repeats its key."""
    lines = ["[%s]" % name]
    for key, v in sorted(keys.items()):
        for item in (v if type(v) == "list" else [v]):
            lines.append("\t%s = %s" % (key, value(item)))
    return lines

def gitconfig(settings):
    """Settings as `section.key` or `section.subsection.key`, the way `git config` names them."""
    lines = []

    # Git takes the last value of a key, so includes come last to override what's above them.
    order = lambda item: (item[0] in ["include", "includeIf"], item[0])
    for section, keys in sorted(settings.items(), key = order):
        plain = {k: v for k, v in keys.items() if type(v) != "dict"}
        if plain:
            lines += section_lines(section, plain)
        for subsection, sub in sorted(keys.items()):
            if type(sub) == "dict":
                lines += section_lines('%s "%s"' % (section, subsection), sub)
    return "\n".join(lines) + "\n"

include = types.record(
    condition = option(types.str, doc = "As in `includeIf`, such as `gitdir:~/work/`."),
    settings = option(types.freeform, default = {}),
)

def render(ctx):
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
        for name, lines in [("ignore", git["ignores"]), ("attributes", git["attributes"])]:
            if lines:
                ctx.file("~/.config/git/" + name, "\n".join(lines) + "\n", user = user)
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
            includes = option(types.list(include), default = []),
            lfs = option(types.bool, default = False, doc = "Large files with Git LFS."),
            delta = option(
                types.bool,
                default = False,
                doc = "Show diffs with delta. Its own options go in `settings.delta`.",
            ),
            ignores = option(
                types.list(types.str),
                default = [],
                doc = "Patterns every repository ignores.",
            ),
            attributes = option(
                types.list(types.str),
                default = [],
                doc = "Attributes for every repository, such as `* merge=mergiraf`.",
            ),
        ),
    },
    writes = [],
    render = render,
)
