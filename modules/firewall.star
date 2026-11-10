# A host firewall, rendered as an nftables ruleset.

def render(ctx):
    firewall = ctx.read("networking.firewall")
    rules = [
        "table inet filter {",
        "  chain input {",
        "    type filter hook input priority 0; policy drop;",
        "    ct state established,related accept",
        "    iif lo accept",
    ]
    for name in firewall["trusted_interfaces"]:
        rules.append("    iifname \"%s\" accept" % name)
    for port in firewall["allowed_tcp_ports"]:
        rules.append("    tcp dport %d accept" % port)
    for port in firewall["allowed_udp_ports"]:
        rules.append("    udp dport %d accept" % port)
    rules.append("  }")
    rules.append("}")
    ctx.package("nftables")
    ctx.file("/etc/nftables.conf", lines(rules))
    ctx.unit("nftables.service", {
        "Unit": {"Description": "nftables firewall", "Before": "network-pre.target"},
        "Service": {
            "Type": "oneshot",
            "RemainAfterExit": "yes",
            "ExecStart": "nft -f /etc/nftables.conf",
            "ExecStop": "nft flush ruleset",
        },
        "Install": {"WantedBy": "multi-user.target"},
    }, restart_on = ["/etc/nftables.conf"])

module(
    name = "firewall",
    options = {
        "networking.firewall": types.record(
            allowed_tcp_ports = option(types.set(types.port)),
            allowed_udp_ports = option(types.set(types.port)),
            trusted_interfaces = option(
                types.set(types.str),
                doc = "Interfaces whose traffic is always accepted, such as a VPN's.",
            ),
        ),
    },
    writes = [],
    enable = True,
    render = render,
)
