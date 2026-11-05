// Syncthing instances. Each instance runs as its own unit; the default user is created as a
// system user, and the sync port is opened when asked.

const SYNC_PORT = 22000;

const folder = types.record({
	path: option(types.path),
	devices: option(types.set(types.str), { default: [] }),
});

const instance = types.record({
	user: option(types.str, { default: "syncthing" }),
	port: option(types.port, { default: 8384, doc: "Port of the web interface on localhost." }),
	data_dir: option(types.path, { default: "/var/lib/syncthing" }),
	open_firewall: option(types.bool, { default: false }),
	folders: types.map(folder),
});

export default module({
	name: "syncthing",
	options: { "services.syncthing": types.map(instance) },
	writes: ["users", "networking.firewall.allowed_tcp_ports"],
	render(ctx) {
		let open = false;
		const instances = ctx.read<Record<string, Infer<typeof instance>>>("services.syncthing");
		for (const [name, cfg] of Object.entries(instances)) {
			ctx.package("syncthing");
			ctx.unit(`syncthing-${name}.service`, {
				Unit: { Description: `Syncthing (${name})`, After: "network.target" },
				Service: {
					User: cfg.user,
					ExecStart: `syncthing serve --no-browser --gui-address=127.0.0.1:${cfg.port} --home=${cfg.data_dir}`,
					Restart: "on-failure",
				},
				Install: { WantedBy: "multi-user.target" },
			});
			ctx.file(`${cfg.data_dir}/folders.json`, formats.json(cfg.folders));
			if (cfg.user === "syncthing") {
				ctx.default("users.syncthing", {
					system: true,
					home: cfg.data_dir,
					description: "Syncthing service",
				});
			}
			open = open || cfg.open_firewall;
		}
		if (open) {
			ctx.set("networking.firewall.allowed_tcp_ports", [SYNC_PORT]);
		}
	},
});
