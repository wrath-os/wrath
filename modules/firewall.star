# A host firewall, rendered as an nftables ruleset in a table of its own. Other tables, such as
# tailscale's, are left alone, and a change replaces this one in a single step.

TABLE = "inet wrath"
CONFIG = "/etc/nftables.conf"

# What IPv6 needs to work at all: finding neighbours and routers, and hearing that a packet was too
# big, which path MTU discovery relies on.
ICMPV6 = [
    "destination-unreachable", "packet-too-big", "time-exceeded", "parameter-problem",
    "nd-router-solicit", "nd-router-advert", "nd-neighbor-solicit", "nd-neighbor-advert",
]
ICMP = ["destination-unreachable", "time-exceeded", "parameter-problem"]

def render(ctx: Context) -> None:
    firewall = ctx.read("networking.firewall")
    icmp, icmpv6 = list(ICMP), list(ICMPV6)
    if firewall["allow_ping"]:
        icmp.append("echo-request")
        icmpv6.append("echo-request")
    rules = [
        # Declared and emptied first, so loading the file replaces the table in one step.
        "table %s" % TABLE,
        "flush table %s" % TABLE,
        "table %s {" % TABLE,
        "  chain input {",
        "    type filter hook input priority 0; policy drop;",
        "    ct state established,related accept",
        "    ct state invalid drop",
        "    iif lo accept",
        "    icmp type { %s } accept" % ", ".join(icmp),
        "    icmpv6 type { %s } accept" % ", ".join(icmpv6),
        # Replies from a DHCPv6 server, which come from its port to the client's link-local one.
        "    ip6 daddr fe80::/64 udp sport 547 udp dport 546 accept",
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
    ctx.file(CONFIG, lines(rules))
    apply = "nft -f " + CONFIG
    ctx.unit("nftables.service", {
        "Unit": {"Description": "nftables firewall", "Before": "network-pre.target"},
        "Service": {
            "Type": "oneshot",
            "RemainAfterExit": "yes",
            "ExecStart": apply,
            "ExecReload": apply,
            "ExecStop": "nft delete table " + TABLE,
        },
        "Install": {"WantedBy": "multi-user.target"},
    })
    # A changed ruleset is loaded over the old one, so there's never a moment with no rules.
    ctx.reload("nftables.service", on = [CONFIG])

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
            allow_ping = option(
                types.bool,
                default = True,
                doc = "Answer pings, over IPv4 and IPv6.",
            ),
        ),
    },
    writes = [],
    enable = True,
    render = render,
)
