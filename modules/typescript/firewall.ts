// A host firewall, rendered as an nftables ruleset.

const firewall = types.record({
	enable: option(types.bool, { default: true }),
	allowed_tcp_ports: option(types.set(types.port), { default: [] }),
	allowed_udp_ports: option(types.set(types.port), { default: [] }),
});

export default module({
	name: "firewall",
	options: { "networking.firewall": firewall },
	writes: [],
	render(ctx) {
		const config = ctx.read<Infer<typeof firewall>>("networking.firewall");
		if (!config.enable) return;
		const rules = [
			"table inet filter {",
			"  chain input {",
			"    type filter hook input priority 0; policy drop;",
			"    ct state established,related accept",
			"    iif lo accept",
			...config.allowed_tcp_ports.map((port) => `    tcp dport ${port} accept`),
			...config.allowed_udp_ports.map((port) => `    udp dport ${port} accept`),
			"  }",
			"}",
		];
		ctx.package("nftables");
		ctx.file("/etc/nftables.conf", rules.join("\n") + "\n");
		ctx.unit("nftables.service", {
			Unit: { Description: "nftables firewall", Before: "network-pre.target" },
			Service: {
				Type: "oneshot",
				RemainAfterExit: "yes",
				ExecStart: "nft -f /etc/nftables.conf",
				ExecStop: "nft flush ruleset",
			},
			Install: { WantedBy: "multi-user.target" },
		});
	},
});
