# Changing who you are on a system built without NixOS: sudo for the `wheel` group, su, and
# passwd, which run as root through wrappers, and the PAM stacks they use.

load("lib/pam.star", "unix_stacks")

SW = "/run/current-system/sw/bin/"

def render(ctx):
    for package in ["sudo", "linux-pam", "shadow", "shadow.su"]:
        ctx.package(package)
    # unix_chkpwd lets PAM check a password for someone who isn't root.
    for name in ["sudo", "su", "passwd", "unix_chkpwd"]:
        ctx.wrapper(name, source = SW + name)
    ctx.file("/etc/sudoers", "root ALL=(ALL:ALL) SETENV: ALL\n%wheel ALL=(ALL:ALL) SETENV: ALL\n", 0o440)

    su = unix_stacks()
    su["auth"] = [{"name": "rootok", "control": "sufficient", "module": "pam_rootok.so"}] + [
        dict(rule, after = "rootok") if rule["name"] == "env" else rule
        for rule in su["auth"]
    ]
    ctx.set("security.pam.services.su", su)
    ctx.set("security.pam.services.passwd", unix_stacks())
    # systemd starts each user's own service manager through this one.
    ctx.set("security.pam.services.systemd-user", unix_stacks())

module(
    name = "accounts",
    options = {},
    writes = [
        "security.pam.services.su",
        "security.pam.services.passwd",
        "security.pam.services.systemd-user",
    ],
    render = render,
    base = True,
)
