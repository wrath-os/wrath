# The default PAM stacks for local logins and sudo.

load("lib/pam.star", "unix_stacks")

def render(ctx: Context) -> None:
    for service in ["login", "sudo"]:
        ctx.set("security.pam.services." + service, unix_stacks())

module(
    name = "pam-base",
    options = {},
    writes = ["security.pam.services.login", "security.pam.services.sudo"],
    render = render,
)
