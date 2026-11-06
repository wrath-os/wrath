# Environment variables for each user's sessions: systemd's user manager and graphical sessions
# read `~/.config/environment.d`, and login shells source `~/.config/distro/session.sh`.

def check(user, name, v):
    where = "users.%s.session_variables.%s" % (user, name)
    if "\n" in v:
        fail("%s: a value can't span lines" % where)
    if "$(" in v or "`" in v:
        fail("%s: a value can refer to variables as `$NAME`, but can't run commands" % where)

def sh_quote(v):
    """Double quotes, so `$NAME` still expands as it does in environment.d."""
    return '"%s"' % v.replace("\\", "\\\\").replace('"', '\\"')

def render(ctx):
    for user, u in ctx.read("users").items():
        variables = u["session_variables"]
        if not variables:
            continue
        conf = []
        sh = []
        for name, v in sorted(variables.items()):
            check(user, name, v)
            conf.append("%s=%s" % (name, v))
            sh.append("export %s=%s" % (name, sh_quote(v)))
        ctx.file("~/.config/environment.d/60-distro.conf", "\n".join(conf) + "\n", user = user)
        ctx.file("~/.config/distro/session.sh", "\n".join(sh) + "\n", user = user)

module(
    name = "session",
    options = {},
    user_options = {
        "session_variables": option(
            types.map(types.str),
            default = {},
            doc = "Variables every session of the user has. Values may refer to others as `$NAME`.",
        ),
    },
    writes = [],
    render = render,
)
