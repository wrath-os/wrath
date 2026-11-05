# Users and groups, rendered as a sysusers.d file.

user = types.record(
    description = option(types.str, optional = True),
    uid = option(types.int, optional = True),
    group = option(types.str, default = "users"),
    extra_groups = option(types.set(types.str), default = []),
    home = option(types.path, optional = True),
    shell = option(types.str, default = "/run/current-system/sw/bin/bash"),
    system = option(types.bool, default = False),
    packages = option(types.set(types.str), default = []),
)

def home_of(name, u):
    if "home" in u:
        return u["home"]
    if u["system"]:
        return "/"
    return "/home/" + name

def render(ctx):
    lines = []
    for name, u in ctx.read("users").items():
        uid = str(u["uid"]) if "uid" in u else "-"
        description = u.get("description", "")
        lines.append('u %s %s "%s" %s %s' % (name, uid, description, home_of(name, u), u["shell"]))
        for group in u["extra_groups"]:
            lines.append("m %s %s" % (name, group))
    ctx.file("/etc/sysusers.d/distro.conf", "\n".join(lines) + "\n")

module(
    name = "users",
    options = {"users": types.map(user)},
    writes = [],
    render = render,
)
