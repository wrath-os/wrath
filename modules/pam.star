# PAM services, rendered as /etc/pam.d files. Other modules add rules to the stacks; rules are
# placed with `before` and `after` anchors.

rule = types.record(
    name = option(types.str),
    control = option(types.str),
    module = option(types.str),
    args = option(types.list(types.str), optional = True),
)

def stack() -> Option:
    return option(types.ordered(rule))

KINDS = ["account", "auth", "password", "session"]

def render(ctx: Context) -> None:
    for service, stacks in ctx.read("security.pam.services").items():
        rules = []
        for kind in KINDS:
            for r in stacks[kind]:
                line = "%s %s %s" % (kind, r["control"], r["module"])
                if "args" in r:
                    line += " " + " ".join(r["args"])
                rules.append(line)
        ctx.file("/etc/pam.d/" + service, lines(rules))

module(
    name = "pam",
    options = {
        "security.pam.services": types.map(types.record(
            account = stack(),
            auth = stack(),
            password = stack(),
            session = stack(),
        )),
    },
    writes = [],
    render = render,
)
