# nginx instances, each with its own config file and unit. Sites become server blocks.

site = types.record(
    domains = option(types.set(types.str), default = []),
    root = option(types.path, optional = True),
    proxy = option(types.str, optional = True),
)

instance = types.record(
    enable = option(types.bool, default = False),
    port = option(types.port, default = 80),
    open_firewall = option(types.bool, default = False),
    sites = types.map(site),
)

def server(port, s):
    lines = [
        "  server {",
        "    listen %d;" % port,
        "    server_name " + " ".join(s["domains"]) + ";",
    ]
    if "root" in s:
        lines.append("    root " + s["root"] + ";")
    if "proxy" in s:
        lines.append("    location / { proxy_pass " + s["proxy"] + "; }")
    lines.append("  }")
    return "\n".join(lines)

def render(ctx):
    ports = []
    for name, cfg in ctx.read("services.nginx").items():
        if not cfg["enable"]:
            continue
        servers = [server(cfg["port"], s) for s in cfg["sites"].values()]
        path = "/etc/nginx/%s.conf" % name
        ctx.package("nginx")
        ctx.file(path, "events {}\nhttp {\n" + "\n".join(servers) + "\n}\n")
        ctx.unit("nginx-%s.service" % name, {
            "Unit": {"Description": "nginx (%s)" % name, "After": "network.target"},
            "Service": {
                "ExecStart": "nginx -c %s -g 'daemon off;'" % path,
                "ExecReload": "nginx -c %s -s reload" % path,
                "Restart": "on-failure",
            },
            "Install": {"WantedBy": "multi-user.target"},
        })
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
