# Changing who you are: sudo for the `wheel` group, su, and
# passwd, which run as root through wrappers, runuser, and the PAM stacks they use.

load("lib/home.star", "home_of")
load("lib/pam.star", "unix_stacks")

SW = "/run/system/bin/"

def account_files(users: dict[str, typing.Any]) -> tuple[str, ...]:
    """/etc/passwd, /etc/group and /etc/shadow for root, nobody and `users`, who get uids from
    1000 in the order of their names, the `users` group, and groups from 400 for their
    `extra_groups` beyond `wheel`. A password set on the machine is kept by activation."""
    passwd = ["root:x:0:0:root:/root:/bin/sh", "nobody:x:65534:65534:nobody:/:/bin/sh"]
    shadow = ["root:*:1::::::", "nobody:*:1::::::"]
    members = {}
    for index, name in enumerate(sorted(users)):
        u = users[name]
        for extra in sorted(u["extra_groups"]):
            members[extra] = members.get(extra, []) + [name]
        passwd.append("%s:x:%d:100:%s:%s:%s" % (
            name,
            1000 + index,
            u.get("description", ""),
            home_of(name, u),
            u.get("shell", SW + "bash"),
        ))
        shadow.append("%s:%s:1::::::" % (name, u.get("initial_hashed_password", "!")))
    group = [
        "root:x:0:",
        "wheel:x:10:" + ",".join(members.pop("wheel", [])),
        "nogroup:x:65534:",
        "users:x:100:",
    ]
    for gid, name in enumerate(sorted(members), 400):
        group.append("%s:x:%d:%s" % (name, gid, ",".join(members[name])))
    return lines(passwd), lines(group), lines(shadow)

def render(ctx: Context) -> None:
    for package in ["sudo", "linux-pam", "shadow"]:
        ctx.package(package)
    passwd, group, shadow = account_files(ctx.read("users"))
    ctx.file("/etc/passwd", passwd, 0o644)
    ctx.file("/etc/group", group, 0o644)
    ctx.file("/etc/shadow", shadow, 0o600)
    # unix_chkpwd lets PAM check a password for someone who isn't root.
    for name in ["sudo", "su", "passwd", "unix_chkpwd"]:
        ctx.wrapper(name, source = SW + name)
    ctx.file(
        "/etc/sudoers",
        "root ALL=(ALL:ALL) SETENV: ALL\n%wheel ALL=(ALL:ALL) SETENV: ALL\n",
        0o440,
    )

    su = unix_stacks()
    su["auth"] = [{"name": "rootok", "control": "sufficient", "module": "pam_rootok.so"}] + [
        dict(rule, after = "rootok") if rule["name"] == "env" else rule
        for rule in su["auth"]
    ]
    ctx.set("security.pam.services.su", su)
    ctx.set("security.pam.services.passwd", unix_stacks())
    # systemd starts each user's own service manager through this one.
    ctx.set("security.pam.services.systemd-user", unix_stacks())
    # root running something as another user, which needs no password.
    runuser = {
        "auth": [{"name": "rootok", "control": "sufficient", "module": "pam_rootok.so"}],
        "account": [{"name": "unix", "control": "required", "module": "pam_unix.so"}],
        "session": [{"name": "unix", "control": "required", "module": "pam_unix.so"}],
    }
    ctx.set("security.pam.services.runuser", runuser)
    ctx.set("security.pam.services.runuser-l", runuser)

module(
    name = "accounts",
    options = {},
    writes = [
        "security.pam.services.su",
        "security.pam.services.passwd",
        "security.pam.services.systemd-user",
        "security.pam.services.runuser",
        "security.pam.services.runuser-l",
    ],
    render = render,
)
