-- Syncthing instances. Each instance runs as its own unit; the default user is created as a
-- system user, and the sync port is opened when asked.

local SYNC_PORT = 22000

local folder = types.record({
	path = option(types.path),
	devices = option(types.set(types.str), { default = {} }),
})

local instance = types.record({
	user = option(types.str, { default = "syncthing" }),
	port = option(types.port, { default = 8384, doc = "Port of the web interface on localhost." }),
	data_dir = option(types.path, { default = "/var/lib/syncthing" }),
	open_firewall = option(types.bool, { default = false }),
	folders = types.map(folder),
})

return {
	name = "syncthing",
	options = {
		["services.syncthing"] = types.map(instance),
	},
	writes = { "users", "networking.firewall.allowed_tcp_ports" },
	render = function(ctx)
		local open = false
		for name, cfg in pairs(ctx.read("services.syncthing")) do
			ctx.package("syncthing")
			ctx.unit("syncthing-" .. name .. ".service", {
				Unit = { Description = "Syncthing (" .. name .. ")", After = "network.target" },
				Service = {
					User = cfg.user,
					ExecStart = string.format(
						"syncthing serve --no-browser --gui-address=127.0.0.1:%d --home=%s",
						cfg.port,
						cfg.data_dir
					),
					Restart = "on-failure",
				},
				Install = { WantedBy = "multi-user.target" },
			})
			ctx.file(cfg.data_dir .. "/folders.json", formats.json(cfg.folders))
			if cfg.user == "syncthing" then
				ctx.default("users.syncthing", {
					system = true,
					home = cfg.data_dir,
					description = "Syncthing service",
				})
			end
			open = open or cfg.open_firewall
		end
		if open then
			ctx.set("networking.firewall.allowed_tcp_ports", { SYNC_PORT })
		end
	end,
}
