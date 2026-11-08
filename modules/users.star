# Users. The backend creates them and their home directories. Everything but `packages` belongs to
# the account, which only a system configuration sets.

load("lib/home.star", "home_of")

user = types.record(
    description = option(types.str, optional = True, account = True),
    uid = option(types.int, optional = True, account = True),
    group = option(types.str, default = "users", account = True),
    extra_groups = option(types.set(types.str), default = [], account = True),
    home = option(types.path, optional = True, account = True),
    shell = option(
        types.str,
        optional = True,
        account = True,
        doc = "The backend's default shell when unset.",
    ),
    system = option(types.bool, default = False, account = True),
    packages = option(types.set(types.str), default = []),
    linger = option(
        types.bool,
        default = False,
        account = True,
        doc = "Start the user's services at boot instead of at their first login.",
    ),
)

def render(ctx):
    users = {}
    for name, u in ctx.read("users").items():
        record = {
            "description": u.get("description", ""),
            "group": u["group"],
            "extra_groups": u["extra_groups"],
            "home": home_of(name, u),
            "system": u["system"],
            "linger": u["linger"],
        }
        for key in ["uid", "shell"]:
            if key in u:
                record[key] = u[key]
        users[name] = record
        for package in u["packages"]:
            ctx.package(package, user = name)
    ctx.base("users", users)

module(
    name = "users",
    options = {"users": types.map(user)},
    writes = [],
    render = render,
    configurations = ["system", "home"],
)
