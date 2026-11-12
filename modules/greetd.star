# The login manager for a desktop: greetd on the first virtual
# terminal, with tuigreet asking who logs in, or the autologin user's session started directly.

SESSIONS = {"niri": "niri-session"}

def render(ctx: Context) -> None:
    desktop = ctx.read("desktop")
    if "session" not in desktop:
        return
    command = SESSIONS[desktop["session"]]
    ctx.package("greetd")
    ctx.package("tuigreet")
    config = {
        "terminal": {"vt": 1},
        "default_session": {"command": "tuigreet --cmd " + command, "user": "greeter"},
    }
    if "autologin" in desktop:
        config["initial_session"] = {"command": command, "user": desktop["autologin"]}
    ctx.file("/etc/greetd/config.toml", formats.toml(config))
    ctx.file("/etc/sysusers.d/greetd.conf", "u greeter - \"greetd greeter\" /var/lib/greeter\n")
    ctx.file("/etc/tmpfiles.d/greetd.conf", "d /var/lib/greeter 0700 greeter greeter -\n")
    ctx.unit("greetd.service", {
        "Unit": {
            "Description": "greetd, the login manager",
            "Wants": "systemd-user-sessions.service",
            # Restarting it would end the desktop session, and a switch run from in it.
            "X-RestartIfChanged": "false",
            "After": "systemd-user-sessions.service getty@tty1.service",
            "Conflicts": "getty@tty1.service",
        },
        "Service": {
            "ExecStart": "greetd --config /etc/greetd/config.toml",
            "Type": "idle",
            "Restart": "on-success",
            "IgnoreSIGPIPE": "no",
            "SendSIGHUP": "yes",
            "TimeoutStopSec": "30s",
            "KeyringMode": "shared",
        },
        "Install": {"WantedBy": "multi-user.target"},
    }, restart_on = ["/etc/greetd/config.toml"])
    login = lambda control: [{"name": "login", "control": control, "module": "login"}]
    ctx.set("security.pam.services.greetd", {
        "auth": login("substack"),
        "account": login("include"),
        "password": login("substack"),
        "session": login("include"),
    })

module(
    name = "greetd",
    options = {},
    writes = ["security.pam.services.greetd"],
    render = render,
)
