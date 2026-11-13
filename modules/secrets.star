# Secrets kept encrypted with age next to the configuration and decrypted on the machine when
# the system starts or switches, to `/run/secrets/<name>`. An option that takes a path accepts
# `{ secret = "name" }` and gets that path, and a unit that mentions it restarts when the
# secret changes.

secret = types.record(
    file = option(
        types.str,
        optional = True,
        doc = "The encrypted file, relative to the configuration. Defaults to `secrets/<name>.age`.",
    ),
    owner = option(types.str, default = "root"),
    group = option(types.str, optional = True, doc = "Defaults to the owner's group."),
    mode = option(types.int, default = 0o400),
    path = option(
        types.path,
        optional = True,
        doc = "Another place to link the decrypted file from, such as a key in a home directory.",
    ),
)

def render(ctx: Context) -> None:
    secrets = ctx.read("secrets")
    for name, s in secrets.items():
        ctx.secret(
            name,
            file = s.get("file", "secrets/%s.age" % name),
            owner = s["owner"],
            group = s.get("group"),
            mode = s["mode"],
            path = s.get("path"),
        )
    if secrets:
        ctx.package("age")
    ctx.base("secrets", {"identities": ctx.read("system.secret_identities")})

module(
    name = "secrets",
    options = {
        "secrets": types.map(secret),
        "system.secret_identities": option(
            types.list(types.path),
            default = ["/etc/ssh/ssh_host_ed25519_key"],
            doc = "Private keys on the machine that decrypt its secrets.",
        ),
        "system.secret_recipients": option(
            types.list(types.str),
            doc = "Public keys `wrath secret` encrypts to: the machine's host key, and the " +
                  "keys of whoever edits its secrets.",
        ),
    },
    writes = [],
    render = render,
)
