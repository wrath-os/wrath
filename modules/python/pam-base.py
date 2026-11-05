# The default PAM stacks for local logins and sudo.

def unix_stacks() -> dict:
    return {
        "account": [
            {"name": "unix", "control": "required", "module": "pam_unix.so"},
        ],
        "auth": [
            {"name": "env", "control": "required", "module": "pam_env.so"},
            {
                "name": "unix",
                "control": "sufficient",
                "module": "pam_unix.so",
                "args": ["likeauth", "try_first_pass"],
                "after": "env",
            },
            {"name": "deny", "control": "required", "module": "pam_deny.so", "after": "unix"},
        ],
        "password": [
            {"name": "unix", "control": "sufficient", "module": "pam_unix.so", "args": ["nullok", "yescrypt"]},
        ],
        "session": [
            {"name": "env", "control": "required", "module": "pam_env.so"},
            {"name": "unix", "control": "required", "module": "pam_unix.so", "after": "env"},
            {"name": "systemd", "control": "optional", "module": "pam_systemd.so", "after": "unix"},
        ],
    }

def render(ctx: Ctx) -> None:
    for service in ["login", "sudo"]:
        ctx.set("security.pam.services." + service, unix_stacks())

module(
    name = "pam-base",
    options = {},
    writes = ["security.pam.services.login", "security.pam.services.sudo"],
    render = render,
)
