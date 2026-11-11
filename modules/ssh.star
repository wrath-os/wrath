# The SSH server. It logs people in with their keys: those in
# `authorized_keys`, and any in their own ~/.ssh/authorized_keys.

load("lib/pam.star", "unix_stacks")

HOST_KEY = "/etc/ssh/ssh_host_ed25519_key"
CONFIG = "/etc/ssh/sshd_config"

def render(ctx: Context) -> None:
    cfg = ctx.read("services.ssh")
    for user, u in ctx.read("users").items():
        if u["authorized_keys"]:
            ctx.file("/etc/ssh/authorized_keys.d/" + user, lines(u["authorized_keys"]))
    ctx.file(CONFIG, lines([
        "Port %d" % cfg["port"],
        "HostKey " + HOST_KEY,
        "AuthorizedKeysFile %h/.ssh/authorized_keys /etc/ssh/authorized_keys.d/%u",
        "UsePAM yes",
        "PasswordAuthentication " + ("yes" if cfg["password_authentication"] else "no"),
        "KbdInteractiveAuthentication no",
        "PermitRootLogin prohibit-password",
        "Subsystem sftp internal-sftp",
    ]))
    ctx.file("/etc/sysusers.d/sshd.conf", "u sshd - \"sshd\" /var/empty\n")
    ctx.file("/etc/tmpfiles.d/sshd.conf", "d /var/empty 0555 root root -\n")
    ctx.unit("sshd.service", {
        "Unit": {"Description": "SSH", "After": "network.target"},
        "Service": {
            # A machine without a key made ahead of time makes its own once.
            "ExecStartPre": "/bin/sh -c 'test -e %s || ssh-keygen -q -t ed25519 -N \"\" -f %s'" % (
                HOST_KEY,
                HOST_KEY,
            ),
            # sshd has to be started by its absolute path.
            "ExecStart": "/run/system/bin/sshd -D -f " + CONFIG,
            "KillMode": "process",
            "Restart": "on-failure",
        },
        "Install": {"WantedBy": "multi-user.target"},
    }, restart_on = [CONFIG])
    ctx.set("security.pam.services.sshd", unix_stacks())
    if cfg["open_firewall"]:
        ctx.set("networking.firewall.allowed_tcp_ports", [cfg["port"]])

module(
    name = "ssh",
    options = {
        "services.ssh": types.record(
            port = option(types.port, default = 22),
            password_authentication = option(
                types.bool,
                default = False,
                doc = "Also let people log in with a password.",
            ),
            open_firewall = option(types.bool, default = True),
        ),
    },
    user_options = {
        "authorized_keys": option(
            types.list(types.str),
            doc = "Public keys that can log in as this user over SSH.",
        ),
    },
    writes = ["security.pam.services.sshd", "networking.firewall.allowed_tcp_ports"],
    enable = False,
    render = render,
)
