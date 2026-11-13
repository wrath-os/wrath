# A service as homes and projects write it, with the fields every service manager has, and how it
# becomes a systemd unit. Modules load it with
# `load("lib/service.star", "service", "check_services", "systemd_unit")`.

load("lib/merge.star", "merge")

service = types.record(
    description = option(types.str, optional = True),
    command = option(
        types.list(types.str),
        required = True,
        doc = "The program and its arguments. Programs on PATH need no path.",
    ),
    environment = option(types.map(types.str)),
    restart = option(types.one_of(["on-failure", "always", "never"]), default = "on-failure"),
    needs = option(
        types.set(types.str),
        doc = "Other services of the same home or project, which start first.",
    ),
    ready = option(
        types.str,
        default = "started",
        doc = "When it counts as started: `started`, `exit` for a task that runs once, or " +
              "`tcp:PORT` once something listens there.",
    ),
    autostart = option(
        types.bool,
        default = True,
        doc = "Whether it starts on its own: with the session for a home, with `wrath up` for " +
              "a project. Otherwise it's started by hand.",
    ),
    systemd = option(
        types.freeform,
        default = {},
        doc = "Unit settings only systemd has, by section, such as `Service.Nice`.",
    ),
)

def tcp_port(ready: str) -> int | None:
    """The port of `tcp:PORT`, or None."""
    if not ready.startswith("tcp:"):
        return None
    port = ready[len("tcp:"):]
    if not port.isdigit() or int(port) < 1 or int(port) > 65535:
        return None
    return int(port)

def check_services(
    ctx: Context,
    where: list[str],
    services: dict[str, dict[str, typing.Any]],
) -> None:
    """Reports what's wrong with `services`, found at the option path `where`."""
    for name, s in services.items():
        at = where + [name]
        if not s["command"]:
            ctx.error(at + ["command"], "a service needs a command to run")
        for arg in s["command"]:
            if "\n" in arg or "\r" in arg or "\t" in arg:
                message = "a command's arguments can't hold newlines or tabs"
                ctx.error(at + ["command"], message)
        ready = s["ready"]
        if ready not in ["started", "exit"] and tcp_port(ready) == None:
            message = "`ready` is `started`, `exit` or `tcp:PORT`, not `%s`" % ready
            ctx.error(at + ["ready"], message)
        for other in s["needs"]:
            if other not in services:
                ctx.error(at + ["needs"], "there's no service `%s` here to need" % other)

def systemd_arg(arg: str) -> str:
    """An argument as systemd's ExecStart reads it."""
    if arg and not any([c in arg for c in " \t\"'\\$%;".elems()]):
        return arg
    escaped = arg.replace("\\", "\\\\").replace("\"", "\\\"")
    return "\"%s\"" % escaped.replace("$", "$$").replace("%", "%%")

def systemd_unit(name: str, s: dict[str, typing.Any]) -> dict[str, typing.Any]:
    """The systemd unit for the service `name`."""
    restart = {"on-failure": "on-failure", "always": "always", "never": "no"}[s["restart"]]
    unit = {
        "Unit": {"Description": s.get("description", name)},
        "Service": {
            "ExecStart": " ".join([systemd_arg(arg) for arg in s["command"]]),
            "Restart": restart,
        },
    }
    if s["ready"] == "exit":
        unit["Service"]["Type"] = "oneshot"
        unit["Service"]["RemainAfterExit"] = True
        unit["Service"].pop("Restart")
    port = tcp_port(s["ready"])
    if port != None:
        unit["Service"]["ExecStartPost"] = (
            "bash -c 'until (exec 3<>/dev/tcp/127.0.0.1/%d) 2>/dev/null; do sleep 0.2; done'" % port
        )
    if s["needs"]:
        needed = sorted([other + ".service" for other in s["needs"]])
        unit["Unit"]["After"] = needed
        unit["Unit"]["Requires"] = needed
    if s["environment"]:
        unit["Service"]["Environment"] = [
            "\"%s=%s\"" % (k, v.replace("\\", "\\\\").replace("\"", "\\\""))
            for k, v in sorted(s["environment"].items())
        ]
    if s["autostart"]:
        unit["Install"] = {"WantedBy": ["default.target"]}
    return merge(unit, s["systemd"])
