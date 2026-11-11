# Fingerprint authentication: the fprintd service and a PAM rule tried before the password.

SERVICES = ["login", "sudo"]

def render(ctx: Context) -> None:
    if not ctx.read("services.fprintd.enable"):
        return
    ctx.package("fprintd")
    ctx.unit("fprintd.service", {
        "Unit": {"Description": "Fingerprint authentication daemon"},
        "Service": {"Type": "dbus", "BusName": "net.reactivated.Fprint", "ExecStart": "fprintd"},
    })
    for service in SERVICES:
        ctx.set("security.pam.services.%s.auth" % service, [
            {
                "name": "fprintd",
                "control": "sufficient",
                "module": "pam_fprintd.so",
                "after": "env",
                "before": "unix",
            },
        ])

module(
    name = "fprintd",
    options = {"services.fprintd.enable": option(types.bool, default = False)},
    writes = ["security.pam.services.login.auth", "security.pam.services.sudo.auth"],
    render = render,
)
