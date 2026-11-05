-- nginx instances, each with its own config file and unit. Sites become server blocks.

local site = types.record({
	domains = option(types.set(types.str), { default = {} }),
	root = option(types.path, { optional = true }),
	proxy = option(types.str, { optional = true }),
})

local instance = types.record({
	enable = option(types.bool, { default = false }),
	port = option(types.port, { default = 80 }),
	open_firewall = option(types.bool, { default = false }),
	sites = types.map(site),
})

local function server(port, s)
	local lines = {
		"  server {",
		string.format("    listen %d;", port),
		"    server_name " .. table.concat(s.domains, " ") .. ";",
	}
	if s.root then
		table.insert(lines, "    root " .. s.root .. ";")
	end
	if s.proxy then
		table.insert(lines, "    location / { proxy_pass " .. s.proxy .. "; }")
	end
	table.insert(lines, "  }")
	return table.concat(lines, "\n")
end

return {
	name = "nginx",
	options = {
		["services.nginx"] = types.map(instance),
	},
	writes = { "networking.firewall.allowed_tcp_ports" },
	render = function(ctx)
		local ports = {}
		for name, cfg in pairs(ctx.read("services.nginx")) do
			if cfg.enable then
				local servers = {}
				for _, s in pairs(cfg.sites) do
					table.insert(servers, server(cfg.port, s))
				end
				local path = "/etc/nginx/" .. name .. ".conf"
				ctx.package("nginx")
				ctx.file(path, "events {}\nhttp {\n" .. table.concat(servers, "\n") .. "\n}\n")
				ctx.unit("nginx-" .. name .. ".service", {
					Unit = { Description = "nginx (" .. name .. ")", After = "network.target" },
					Service = {
						ExecStart = "nginx -c " .. path .. " -g 'daemon off;'",
						ExecReload = "nginx -c " .. path .. " -s reload",
						Restart = "on-failure",
					},
					Install = { WantedBy = "multi-user.target" },
				})
				if cfg.open_firewall then
					table.insert(ports, cfg.port)
				end
			end
		end
		if #ports > 0 then
			ctx.set("networking.firewall.allowed_tcp_ports", ports)
		end
	end,
}
