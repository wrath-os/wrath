# Secrets for a home of its own, kept encrypted with age next to its configuration and decrypted
# by the user when the home switches, to `~/.local/state/wrath/secrets/<name>`. A system's secrets
# are under `secrets` in the system configuration instead.

secret = types.record(
    file = option(
        types.str,
        optional = True,
        doc = "The encrypted file, relative to the configuration. Defaults to " +
              "`secrets/<name>.age`.",
    ),
    mode = option(types.int, default = 0o400),
    path = option(
        types.str,
        optional = True,
        doc = "Another place in the home to link the decrypted file from, such as `~/.netrc`.",
    ),
)

def render(ctx: Context) -> None:
    for user, u in ctx.read("users").items():
        secrets = u["secrets"]
        for name, s in secrets.items():
            ctx.secret(
                name,
                file = s.get("file", "secrets/%s.age" % name),
                owner = user,
                mode = s["mode"],
                path = s.get("path"),
            )
        if secrets:
            ctx.package("age", user = user)
        ctx.base("secrets", {"identities": u["secret_identities"]})

module(
    name = "home-secrets",
    options = {},
    user_options = {
        "secrets": types.map(secret),
        "secret_identities": option(
            types.list(types.str),
            default = ["~/.ssh/id_ed25519"],
            doc = "Private keys in the home that decrypt its secrets.",
        ),
        "secret_recipients": option(
            types.list(types.str),
            doc = "Public keys `wrath secret` encrypts to: yours, and any other key that " +
                  "should decrypt this home's secrets.",
        ),
    },
    writes = [],
    render = render,
    configurations = ["home"],
)
