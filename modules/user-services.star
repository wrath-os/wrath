# Services that run for each user, in their systemd user manager. They start with the user's
# first session, or at boot for a user with `linger`. They're written the way a project's are.

load("lib/service.star", "check_services", "service", "systemd_unit")

def render(ctx: Context) -> None:
    for user, u in ctx.read("users").items():
        check_services(ctx, ["users", user, "services"], u["services"])
        for name, s in u["services"].items():
            ctx.unit(name + ".service", systemd_unit(name, s), user = user)

module(
    name = "user-services",
    options = {},
    user_options = {"services": types.map(service)},
    writes = [],
    render = render,
    configurations = ["system", "home"],
)
