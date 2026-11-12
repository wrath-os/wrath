# The machine's identity: hostname and time zone. The backend writes them where the system
# expects them. Without a hostname, the machine uses its name, filled in on the machine when it
# switches, so every machine with a role gets the same build.

def plain_zone(zone: str) -> bool:
    """Whether `zone` is written as tzdata names its zones, such as `Europe/Stockholm`."""
    for part in zone.split("/"):
        letters = part.replace("_", "").replace("-", "").replace("+", "")
        if not part or part == ".." or not letters.isalnum():
            return False
    return True

def render(ctx: Context) -> None:
    system = ctx.read("system")
    hostname = system.get("hostname") or machine_value("machine.name")
    if type(hostname) == "runtime_value":
        ctx.file("/etc/hostname", hostname + "\n", 420)
        hostname = ""
    if not plain_zone(system["timezone"]):
        ctx.error("system.timezone", "name a time zone as tzdata does, such as `Europe/Stockholm`")
    # /etc/localtime links into the zones tzdata shares.
    ctx.package("tzdata")
    ctx.base("system", {"hostname": hostname, "timezone": system["timezone"]})

module(
    name = "system",
    options = {
        "system.hostname": option(
            types.str,
            optional = True,
            runtime = True,
            doc = "The machine's hostname. Without one, the machine's name.",
        ),
        "system.timezone": option(
            types.str,
            default = "UTC",
            doc = "The time zone, as tzdata names it, such as `Europe/Stockholm`.",
        ),
    },
    writes = [],
    render = render,
)
