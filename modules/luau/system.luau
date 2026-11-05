-- The machine's identity: hostname and timezone.

return {
	name = "system",
	options = {
		["system.hostname"] = option(types.str, { doc = "The machine's hostname." }),
		["system.timezone"] = option(types.str, { default = "UTC" }),
	},
	writes = {},
	render = function(ctx)
		local system = ctx.read("system")
		ctx.file("/etc/hostname", system.hostname .. "\n")
		ctx.file("/etc/timezone", system.timezone .. "\n")
	end,
}
