# Users. The backend creates them and their home directories. Everything but `packages` belongs to
# the account, which only a system configuration sets.

load("lib/home.star", "home_of")

user = types.record(
    description = option(types.str, optional = True, account = True),
    uid = option(types.int, optional = True, account = True),
    group = option(types.str, default = "users", account = True),
    extra_groups = option(types.set(types.str), account = True),
    home = option(types.path, optional = True, account = True),
    initial_hashed_password = option(
        types.str,
        optional = True,
        account = True,
        doc = "The password hash a new account starts with. `passwd` on the machine replaces it.",
    ),
    shell = option(
        types.str,
        optional = True,
        account = True,
        doc = "The backend's default shell when unset.",
    ),
    system = option(types.bool, default = False, account = True),
    packages = option(types.set(types.str)),
    linger = option(
        types.bool,
        default = False,
        account = True,
        doc = "Start the user's services at boot instead of at their first login. Without it, " +
              "lingering turned on with `loginctl` is turned off again.",
    ),
)

def render(ctx: Context) -> None:
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
        for key in ["uid", "shell", "initial_hashed_password"]:
            if key in u:
                record[key] = u[key]
        users[name] = record
        for package in u["packages"]:
            ctx.package(package, user = name)
    ctx.base("users", users)
    linger(ctx, users)

def linger(ctx: Context, users: dict[str, typing.Any]) -> None:
    """Tells logind which users' service managers run from boot. The unit changes, and so runs
    again on a switch, whenever someone's `linger` does."""
    lingering = sorted([name for name, u in users.items() if u["linger"]])
    others = sorted([name for name, u in users.items() if not u["linger"] and not u["system"]])
    loginctl = "/run/system/systemd/bin/loginctl"
    commands = [loginctl + " enable-linger " + name for name in lingering]
    commands += [loginctl + " disable-linger " + name for name in others]
    if not commands:
        return
    ctx.unit("linger.service", {
        "Unit": {
            "Description": "Users whose services start at boot",
            "After": "systemd-logind.service systemd-user-sessions.service",
        },
        "Service": {"Type": "oneshot", "RemainAfterExit": "yes", "ExecStart": commands},
        "Install": {"WantedBy": "multi-user.target"},
    })

module(
    name = "users",
    options = {"users": types.map(user)},
    writes = [],
    render = render,
    configurations = ["system", "home"],
)
