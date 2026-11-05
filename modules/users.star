# Users. The backend creates them and their home directories.

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
        return "/var/empty"
    return "/home/" + name

def render(ctx):
    users = {}
    for name, u in ctx.read("users").items():
        record = {
            "description": u.get("description", ""),
            "group": u["group"],
            "extra_groups": u["extra_groups"],
            "home": home_of(name, u),
            "shell": u["shell"],
            "system": u["system"],
            "packages": u["packages"],
        }
        if "uid" in u:
            record["uid"] = u["uid"]
        users[name] = record
    ctx.base("users", users)

module(
    name = "users",
    options = {"users": types.map(user)},
    writes = [],
    render = render,
)
