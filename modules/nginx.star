# nginx instances, each with its own config file and unit. Sites become server blocks.

site = types.record(
    domains = option(types.set(types.str)),
    root = option(types.path, optional = True),
    proxy = option(types.str, optional = True),
)

instance = types.record(
    enable = option(types.bool, default = False),
    port = option(types.port, default = 80),
    open_firewall = option(types.bool, default = False),
    sites = types.map(site),
)

def server(port: int, s: dict[str, typing.Any]) -> str:
    out = [
        "  server {",
        "    listen %d;" % port,
        "    server_name " + " ".join(s["domains"]) + ";",
    ]
    if "root" in s:
        out.append("    root " + s["root"] + ";")
    if "proxy" in s:
        out.append("    location / { proxy_pass " + s["proxy"] + "; }")
    out.append("  }")
    return "\n".join(out)

def render(ctx: Context) -> None:
    ports = []
    for name, cfg in ctx.read("services.nginx").items():
        if not cfg["enable"]:
            continue
        servers = [server(cfg["port"], s) for s in cfg["sites"].values()]
        path = "/etc/nginx/%s.conf" % name
        ctx.package("nginx")
        logs = "/var/log/nginx/" + name
        ctx.file(path, lines([
            "pid /run/nginx-%s/nginx.pid;" % name,
            "error_log %s-error.log;" % logs,
            "events {}",
            "http {",
            "  access_log %s-access.log;" % logs,
        ] + servers + ["}"]))
        ctx.unit("nginx-%s.service" % name, {
            "Unit": {"Description": "nginx (%s)" % name, "After": "network.target"},
            "Service": {
                "ExecStart": "nginx -c %s -g 'daemon off;'" % path,
                "ExecReload": "nginx -c %s -s reload" % path,
                "Restart": "on-failure",
                "RuntimeDirectory": "nginx-%s" % name,
                "LogsDirectory": "nginx",
                "PrivateTmp": "yes",
            },
            "Install": {"WantedBy": "multi-user.target"},
        }, restart_on = [path])
        if cfg["open_firewall"]:
            ports.append(cfg["port"])
    if ports:
        ctx.set("networking.firewall.allowed_tcp_ports", ports)

module(
    name = "nginx",
    options = {"services.nginx": types.map(instance)},
    writes = ["networking.firewall.allowed_tcp_ports"],
    render = render,
)
