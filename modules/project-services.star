# A project's services, such as its database, which `wrath up` runs while you work on it. They're
# written the way a home's are.

load("lib/service.star", "check_services", "service")

def render(ctx: Context) -> None:
    check_services(ctx, ["services"], ctx.read("services"))

module(
    name = "project-services",
    options = {"services": types.map(service)},
    writes = [],
    render = render,
    configurations = ["project"],
)
