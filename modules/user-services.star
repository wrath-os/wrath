# Services that run in each user's systemd user manager. They start with the user's first
# session, or at boot for a user with `linger`.

def merge(a, b):
    out = dict(a)
    for key, v in b.items():
        if type(v) == "dict" and type(out.get(key)) == "dict":
            out[key] = merge(out[key], v)
        else:
            out[key] = v
    return out

service = types.record(
    description = option(types.str, optional = True),
    exec = option(types.str, doc = "The command, with arguments. Commands on PATH need no path."),
    environment = option(types.map(types.str), default = {}),
    restart = option(types.str, default = "on-failure"),
    after = option(types.list(types.str), default = []),
    wanted_by = option(
        types.list(types.str),
        default = ["default.target"],
        doc = "Targets that start it. `[]` leaves it to be started by hand.",
    ),
    settings = option(
        types.freeform,
        default = {},
        doc = "Anything else in the unit, by section, such as `Service.Nice`.",
    ),
)

def render(ctx):
    for user, u in ctx.read("users").items():
        for name, s in u["services"].items():
            unit = {
                "Unit": {"Description": s.get("description", name)},
                "Service": {
                    "ExecStart": s["exec"],
                    "Restart": s["restart"],
                },
            }
            if s["after"]:
                unit["Unit"]["After"] = s["after"]
            if s["environment"]:
                unit["Service"]["Environment"] = [
                    "\"%s=%s\"" % (k, v.replace("\\", "\\\\").replace("\"", "\\\""))
                    for k, v in sorted(s["environment"].items())
                ]
            if s["wanted_by"]:
                unit["Install"] = {"WantedBy": s["wanted_by"]}
            ctx.unit(name + ".service", merge(unit, s["settings"]), user = user)

module(
    name = "user-services",
    options = {},
    user_options = {"services": types.map(service)},
    writes = [],
    render = render,
)
