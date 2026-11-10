# What survives a reboot on a machine whose root is erased at every boot. With `state.path` set,
# `/` is memory, and only the directories and files listed here are kept, under that path. It
# leaves a machine with exactly what its configuration lists.

load("lib/home.star", "home_of")

ALWAYS_KEPT = ["/etc/distro", "/var/lib/distro"]
ACCOUNTS = "/var/lib/distro/accounts"

kept = types.record(
    directories = option(types.set(types.str)),
    files = option(types.set(types.str)),
)

def check_inside(ctx, at, path):
    if ".." in path.split("/"):
        ctx.error(at, "`%s` goes outside where it's kept" % path)

def check_absolute(ctx, at, path):
    check_inside(ctx, at, path)
    if not path.startswith("/") or path == "/":
        ctx.error(at, "`%s` isn't an absolute path below `/`" % path)

def check_relative(ctx, at, path):
    check_inside(ctx, at, path)
    if not path or path.startswith("/") or path.startswith("~"):
        ctx.error(at, "`%s` is a path inside the user's home, such as `.ssh`" % path)

def check_identities(ctx, directories, files):
    """The keys secrets are decrypted with have to be there after a reboot."""
    if not ctx.read("secrets"):
        return
    for identity in ctx.read("system.secret_identities"):
        inside = [d for d in directories if identity.startswith(d + "/")]
        if identity not in files and not inside:
            ctx.error("state.files", "secrets are decrypted with `%s`, which isn't kept" % identity)

def render(ctx):
    state = ctx.read("state")
    users = ctx.read("users")
    directories = sorted(state["directories"])
    files = sorted(state["files"])
    homes = []
    for name, user in users.items():
        if user["state"]["directories"] or user["state"]["files"]:
            homes.append({
                "user": name,
                "group": user["group"],
                "home": home_of(name, user),
                "directories": sorted(user["state"]["directories"]),
                "files": sorted(user["state"]["files"]),
            })
    if "path" not in state:
        if directories or files or homes:
            ctx.error("state.directories", "keeping paths needs a `state.path` to keep them in")
        return
    for path in directories:
        check_absolute(ctx, "state.directories", path)
    for path in files:
        check_absolute(ctx, "state.files", path)
    for name, user in users.items():
        for key in ["directories", "files"]:
            for path in user["state"][key]:
                check_relative(ctx, ["users", name, "state", key], path)
    check_identities(ctx, directories, files)
    ctx.base("state", {
        "path": state["path"],
        "directories": sorted({path: True for path in directories + ALWAYS_KEPT}),
        "files": files,
        "homes": homes,
    })
    if "/etc/machine-id" in files:
        # A kept id is a mount, which this service takes for a temporary id to write to disk.
        ctx.unit("systemd-machine-id-commit.service.d/distro.conf", {
            "Unit": {"ConditionPathExists": "!/etc/machine-id"},
        })
    # Passwords changed since the last switch are kept too.
    ctx.unit("distro-save-accounts.service", {
        "Unit": {"Description": "Keep users and passwords for the next boot"},
        "Service": {
            "Type": "oneshot",
            "RemainAfterExit": True,
            "ExecStart": "true",
            "ExecStop": "cp -p /etc/passwd /etc/group /etc/shadow " + ACCOUNTS,
        },
        "Install": {"WantedBy": "multi-user.target"},
    })

module(
    name = "state",
    options = {
        "state.directories": option(types.set(types.str), doc = "Kept as they are."),
        "state.files": option(types.set(types.str)),
    },
    user_options = {"state": option(kept, default = {})},
    writes = [],
    render = render,
)
