# Syncthing instances. Each instance runs as its own unit; the default user is created as a
# system user, and the sync port is opened when asked.

SYNC_PORT = 22000

folder = types.record(
    path = option(types.path),
    devices = option(types.set(types.str), default = []),
)

instance = types.record(
    user = option(types.str, default = "syncthing"),
    port = option(types.port, default = 8384, doc = "Port of the web interface on localhost."),
    data_dir = option(types.path, default = "/var/lib/syncthing"),
    open_firewall = option(types.bool, default = False),
    folders = types.map(folder),
)

def render(ctx: Ctx) -> None:
    open = False
    for name, cfg in ctx.read("services.syncthing").items():
        ctx.package("syncthing")
        ctx.unit("syncthing-%s.service" % name, {
            "Unit": {"Description": "Syncthing (%s)" % name, "After": "network.target"},
            "Service": {
                "User": cfg["user"],
                "ExecStart": "syncthing serve --no-browser --gui-address=127.0.0.1:%d --home=%s" % (
                    cfg["port"],
                    cfg["data_dir"],
                ),
                "Restart": "on-failure",
            },
            "Install": {"WantedBy": "multi-user.target"},
        })
        ctx.file(cfg["data_dir"] + "/folders.json", formats.json(cfg["folders"]))
        if cfg["user"] == "syncthing":
            ctx.default("users.syncthing", {
                "system": True,
                "home": cfg["data_dir"],
                "description": "Syncthing service",
            })
        open = open or cfg["open_firewall"]
    if open:
        ctx.set("networking.firewall.allowed_tcp_ports", [SYNC_PORT])

module(
    name = "syncthing",
    options = {"services.syncthing": types.map(instance)},
    writes = ["users", "networking.firewall.allowed_tcp_ports"],
    render = render,
)
