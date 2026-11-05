# A host firewall, rendered as an nftables ruleset.

def render(ctx: Ctx) -> None:
    firewall = ctx.read("networking.firewall")
    if not firewall["enable"]:
        return
    rules = [
        "table inet filter {",
        "  chain input {",
        "    type filter hook input priority 0; policy drop;",
        "    ct state established,related accept",
        "    iif lo accept",
    ]
    for port in firewall["allowed_tcp_ports"]:
        rules.append("    tcp dport %d accept" % port)
    for port in firewall["allowed_udp_ports"]:
        rules.append("    udp dport %d accept" % port)
    rules.append("  }")
    rules.append("}")
    ctx.package("nftables")
    ctx.file("/etc/nftables.conf", "\n".join(rules) + "\n")
    ctx.unit("nftables.service", {
        "Unit": {"Description": "nftables firewall", "Before": "network-pre.target"},
        "Service": {
            "Type": "oneshot",
            "RemainAfterExit": "yes",
            "ExecStart": "nft -f /etc/nftables.conf",
            "ExecStop": "nft flush ruleset",
        },
        "Install": {"WantedBy": "multi-user.target"},
    })

module(
    name = "firewall",
    options = {
        "networking.firewall": types.record(
            enable = option(types.bool, default = True),
            allowed_tcp_ports = option(types.set(types.port), default = []),
            allowed_udp_ports = option(types.set(types.port), default = []),
        ),
    },
    writes = [],
    render = render,
)
