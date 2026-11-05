# PAM services, rendered as /etc/pam.d files. Other modules add rules to the stacks; rules are
# placed with `before` and `after` anchors.

rule = types.record(
    name = option(types.str),
    control = option(types.str),
    module = option(types.str),
    args = option(types.list(types.str), optional = True),
)

def stack():
    return option(types.ordered(rule), default = [])

KINDS = ["account", "auth", "password", "session"]

def render(ctx):
    for service, stacks in ctx.read("security.pam.services").items():
        lines = []
        for kind in KINDS:
            for r in stacks[kind]:
                line = "%s %s %s" % (kind, r["control"], r["module"])
                if "args" in r:
                    line += " " + " ".join(r["args"])
                lines.append(line)
        ctx.file("/etc/pam.d/" + service, "\n".join(lines) + "\n")

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
