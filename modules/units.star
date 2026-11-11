# systemd units written straight in the configuration, with systemd's own sections and settings:
# services of the machine's own, and the timers that run them.

KINDS = [
    "automount",
    "mount",
    "path",
    "service",
    "slice",
    "socket",
    "swap",
    "target",
    "timer",
]

def is_unit_name(name: str) -> bool:
    """Whether `name` is a unit, or a drop-in for one such as `nginx.service.d/limits.conf`."""
    unit, slash, dropin = name.partition(".d/")
    if slash and not dropin.endswith(".conf"):
        return False
    stem, _, kind = unit.rpartition(".")
    return bool(stem) and kind in KINDS

def render(ctx: Context) -> None:
    for name, sections in ctx.read("units").items():
        at = ["units", name]
        if not is_unit_name(name):
            ctx.error(at, "`%s` isn't a unit name, which ends in a kind such as `.service`" % name)
        for section, settings in sections.items():
            if type(settings) != "dict":
                ctx.error(at + [section], "`%s` isn't a section, such as `Service`" % section)
        if name.endswith(".timer") and "Install" not in sections:
            sections = dict(sections, Install = {"WantedBy": "timers.target"})
        ctx.unit(name, sections)

module(
    name = "units",
    options = {
        "units": option(
            types.map(types.freeform),
            doc = "systemd units by name, each with its sections, such as `Service.ExecStart`. " +
                  "A timer starts with the machine unless it has an `Install` section.",
        ),
    },
    writes = [],
    render = render,
    configurations = ["system"],
)
