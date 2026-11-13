# polkit, which decides what unprivileged programs may ask privileged services to do, such as a
# desktop shutting down or mounting a disk. Members of `wheel` are its administrators.

load("lib/pam.star", "unix_stacks")

def render(ctx: Context) -> None:
    ctx.service_package("polkit")
    ctx.package("polkit")
    # Agents check a user's password through this helper, started for each request.
    ctx.enable("polkit-agent-helper.socket")
    ctx.wrapper("pkexec", source = "/run/system/bin/pkexec")
    ctx.file(
        "/etc/polkit-1/rules.d/50-wrath.rules",
        "polkit.addAdminRule(function (action, subject) {\n" +
        "  return [\"unix-group:wheel\"];\n" +
        "});\n",
    )
    ctx.set("security.pam.services.polkit-1", unix_stacks())

module(
    name = "polkit",
    options = {},
    writes = ["security.pam.services.polkit-1"],
    render = render,
)
