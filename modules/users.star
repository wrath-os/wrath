# Users. The backend creates them and their home directories.

user = types.record(
    description = option(types.str, optional = True),
    uid = option(types.int, optional = True),
    group = option(types.str, default = "users"),
    extra_groups = option(types.set(types.str), default = []),
    home = option(types.path, optional = True),
    shell = option(types.str, optional = True, doc = "The backend's default shell when unset."),
    system = option(types.bool, default = False),
    packages = option(types.set(types.str), default = []),
    linger = option(
        types.bool,
        default = False,
        doc = "Start the user's services at boot instead of at their first login.",
    ),
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
            "system": u["system"],
            "packages": u["packages"],
            "linger": u["linger"],
        }
        for key in ["uid", "shell"]:
            if key in u:
                record[key] = u[key]
        users[name] = record
    ctx.base("users", users)

module(
    name = "users",
    options = {"users": types.map(user)},
    writes = [],
    render = render,
)
