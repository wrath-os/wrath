-- A host firewall, rendered as an nftables ruleset.

return {
	name = "firewall",
	options = {
		["networking.firewall"] = types.record({
			enable = option(types.bool, { default = true }),
			allowed_tcp_ports = option(types.set(types.port), { default = {} }),
			allowed_udp_ports = option(types.set(types.port), { default = {} }),
		}),
	},
	writes = {},
	render = function(ctx)
		local firewall = ctx.read("networking.firewall")
		if not firewall.enable then
			return
		end
		local rules = {
			"table inet filter {",
			"  chain input {",
			"    type filter hook input priority 0; policy drop;",
			"    ct state established,related accept",
			"    iif lo accept",
		}
		for _, port in ipairs(firewall.allowed_tcp_ports) do
			table.insert(rules, string.format("    tcp dport %d accept", port))
		end
		for _, port in ipairs(firewall.allowed_udp_ports) do
			table.insert(rules, string.format("    udp dport %d accept", port))
		end
		table.insert(rules, "  }")
		table.insert(rules, "}")
		ctx.package("nftables")
		ctx.file("/etc/nftables.conf", table.concat(rules, "\n") .. "\n")
		ctx.unit("nftables.service", {
			Unit = { Description = "nftables firewall", Before = "network-pre.target" },
			Service = {
				Type = "oneshot",
				RemainAfterExit = "yes",
				ExecStart = "nft -f /etc/nftables.conf",
				ExecStop = "nft flush ruleset",
			},
			Install = { WantedBy = "multi-user.target" },
		})
	end,
}
