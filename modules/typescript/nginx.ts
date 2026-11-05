// nginx instances, each with its own config file and unit. Sites become server blocks.

const site = types.record({
	domains: option(types.set(types.str), { default: [] }),
	root: option(types.path, { optional: true }),
	proxy: option(types.str, { optional: true }),
});

const instance = types.record({
	enable: option(types.bool, { default: false }),
	port: option(types.port, { default: 80 }),
	open_firewall: option(types.bool, { default: false }),
	sites: types.map(site),
});

function server(port: number, s: Infer<typeof site>): string {
	const lines = ["  server {", `    listen ${port};`, `    server_name ${s.domains.join(" ")};`];
	if (s.root !== undefined) lines.push(`    root ${s.root};`);
	if (s.proxy !== undefined) lines.push(`    location / { proxy_pass ${s.proxy}; }`);
	lines.push("  }");
	return lines.join("\n");
}

export default module({
	name: "nginx",
	options: { "services.nginx": types.map(instance) },
	writes: ["networking.firewall.allowed_tcp_ports"],
	render(ctx) {
		const ports: number[] = [];
		const instances = ctx.read<Record<string, Infer<typeof instance>>>("services.nginx");
		for (const [name, cfg] of Object.entries(instances)) {
			if (!cfg.enable) continue;
			const servers = Object.values(cfg.sites).map((s) => server(cfg.port, s));
			const path = `/etc/nginx/${name}.conf`;
			ctx.package("nginx");
			ctx.file(path, `events {}\nhttp {\n${servers.join("\n")}\n}\n`);
			ctx.unit(`nginx-${name}.service`, {
				Unit: { Description: `nginx (${name})`, After: "network.target" },
				Service: {
					ExecStart: `nginx -c ${path} -g 'daemon off;'`,
					ExecReload: `nginx -c ${path} -s reload`,
					Restart: "on-failure",
				},
				Install: { WantedBy: "multi-user.target" },
			});
			if (cfg.open_firewall) ports.push(cfg.port);
		}
		if (ports.length > 0) {
			ctx.set("networking.firewall.allowed_tcp_ports", ports);
		}
	},
});
