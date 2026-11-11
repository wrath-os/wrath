# The Tailscale node agent. Logging in stays manual (`tailscale up`), since the login is state
# on the machine rather than configuration.

def render(ctx: Context) -> None:
    cfg = ctx.read("services.tailscale")
    interface = cfg["interface"]
    ctx.package("tailscale")
    # The agent checks kernel modules and lists processes.
    ctx.package("kmod")
    ctx.package("procps")
    flags = ["--tun", interface] + cfg["flags"]
    ctx.unit("tailscaled.service", {
        "Unit": {
            "Description": "Tailscale node agent",
            "Wants": ["network-pre.target", "network-online.target"],
            "After": ["network-pre.target", "network-online.target", "systemd-resolved.service"],
        },
        "Service": {
            "Type": "notify",
            "ExecStart": "tailscaled --state=/var/lib/tailscale/tailscaled.state " +
                         "--socket=/run/tailscale/tailscaled.sock --port=%d %s" % (
                             cfg["port"],
                             " ".join(flags),
                         ),
            "ExecStopPost": "tailscaled --cleanup",
            "Restart": "on-failure",
            # The system has nftables, and no iptables to fall back on.
            "Environment": "TS_DEBUG_FIREWALL_MODE=nftables",
            "RuntimeDirectory": "tailscale",
            "RuntimeDirectoryMode": "0755",
            "StateDirectory": "tailscale",
            "StateDirectoryMode": "0700",
            "CacheDirectory": "tailscale",
            "CacheDirectoryMode": "0750",
        },
        "Install": {"WantedBy": "multi-user.target"},
    })

    # The agent manages its interface itself, so networkd leaves it alone.
    path = "/etc/systemd/network/50-tailscale.network"
    ctx.file(path, formats.systemd({
        "Match": {"Name": interface},
        "Link": {"Unmanaged": True, "ActivationPolicy": "manual"},
    }))
    ctx.reload("systemd-networkd.service", on = [path])

    if cfg["open_firewall"]:
        ctx.set("networking.firewall.allowed_udp_ports", [cfg["port"]])
    if cfg["trust_interface"]:
        ctx.set("networking.firewall.trusted_interfaces", [interface])

module(
    name = "tailscale",
    options = {
        "services.tailscale": types.record(
            interface = option(types.str, default = "tailscale0"),
            port = option(types.port, default = 41641, doc = "UDP port for direct connections."),
            open_firewall = option(types.bool, default = False, doc = "Open the UDP port."),
            trust_interface = option(
                types.bool,
                default = False,
                doc = "Accept all traffic arriving over the tailnet.",
            ),
            flags = option(types.list(types.str), doc = "More flags for tailscaled."),
        ),
    },
    writes = ["networking.firewall.allowed_udp_ports", "networking.firewall.trusted_interfaces"],
    enable = False,
    render = render,
)
