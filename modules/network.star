# Networks with systemd-networkd and DNS with systemd-resolved, both of which the base system
# provides. This module only writes their configuration and asks for them to reload or restart
# when it changes.

network = types.record(
    match = option(
        types.list(types.str),
        required = True,
        doc = "Interface names to configure; globs like `enp*` work.",
    ),
    dhcp = option(types.one_of(["yes", "no", "ipv4", "ipv6"]), default = "yes"),
    priority = option(
        types.range(0, 99),
        default = 50,
        doc = "Orders the file among others; the first file matching an interface applies.",
    ),
    ipv6_privacy = option(types.bool, default = False, doc = "Use temporary IPv6 addresses."),
    accept_ra = option(types.bool, optional = True, doc = "Accept IPv6 router advertisements."),
    ignore_carrier_loss = option(
        types.str,
        optional = True,
        doc = "How long to keep the configuration when the link drops, such as `3s`.",
    ),
    sections = types.map(types.map(types.str)),
)

DNS_KEYS = {
    "dnssec": "DNSSEC",
    "dns_over_tls": "DNSOverTLS",
    "cache": "Cache",
}

def render_network(ctx: Context, name: str, cfg: dict[str, typing.Any]) -> str:
    sections = {
        "Match": {"Name": cfg["match"]},
        "Network": {"DHCP": cfg["dhcp"], "IPv6PrivacyExtensions": cfg["ipv6_privacy"]},
    }
    if "accept_ra" in cfg:
        sections["Network"]["IPv6AcceptRA"] = cfg["accept_ra"]
    if "ignore_carrier_loss" in cfg:
        sections["Network"]["IgnoreCarrierLoss"] = cfg["ignore_carrier_loss"]
    for section, keys in cfg["sections"].items():
        sections.setdefault(section, {}).update(keys)
    priority = str(cfg["priority"])
    path = "/etc/systemd/network/%s-%s.network" % ("0" * (2 - len(priority)) + priority, name)
    ctx.file(path, formats.systemd(sections))
    return path

def render(ctx: Context) -> None:
    networks = ctx.read("networking.networks")
    paths = [render_network(ctx, name, cfg) for name, cfg in networks.items()]
    if paths:
        ctx.reload("systemd-networkd.service", on = paths)
        # Interfaces get only the networks configured here.
        ctx.default("networking.dhcp", False)

    dns = ctx.read("networking.dns")
    resolve = {key: dns[option] for option, key in DNS_KEYS.items() if option in dns}
    if dns["fallback"]:
        resolve["FallbackDNS"] = " ".join(dns["fallback"])
    if resolve:
        path = "/etc/systemd/resolved.conf.d/wrath.conf"
        ctx.file(path, formats.systemd({"Resolve": resolve}))
        ctx.restart("systemd-resolved.service", on = [path])
        ctx.set("networking.resolved", True)

module(
    name = "network",
    options = {
        "networking.networks": types.map(network),
        "networking.dns": types.record(
            fallback = option(
                types.list(types.str),
                doc = "Servers to use when no network provides one.",
            ),
            dnssec = option(types.one_of(["yes", "no", "allow-downgrade"]), optional = True),
            dns_over_tls = option(types.one_of(["yes", "no", "opportunistic"]), optional = True),
            cache = option(types.one_of(["yes", "no", "no-negative"]), optional = True),
        ),
    },
    writes = ["networking.dhcp", "networking.resolved"],
    render = render,
)
