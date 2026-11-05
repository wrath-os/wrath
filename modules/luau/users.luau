-- Users and groups, rendered as a sysusers.d file.

local user = types.record({
	description = option(types.str, { optional = true }),
	uid = option(types.int, { optional = true }),
	group = option(types.str, { default = "users" }),
	extra_groups = option(types.set(types.str), { default = {} }),
	home = option(types.path, { optional = true }),
	shell = option(types.str, { default = "/run/current-system/sw/bin/bash" }),
	system = option(types.bool, { default = false }),
	packages = option(types.set(types.str), { default = {} }),
})

local function home_of(name, u)
	if u.home then
		return u.home
	elseif u.system then
		return "/"
	else
		return "/home/" .. name
	end
end

return {
	name = "users",
	options = {
		users = types.map(user),
	},
	writes = {},
	render = function(ctx)
		local lines = {}
		for name, u in pairs(ctx.read("users")) do
			local uid = if u.uid then tostring(u.uid) else "-"
			local description = u.description or ""
			table.insert(
				lines,
				string.format('u %s %s "%s" %s %s', name, uid, description, home_of(name, u), u.shell)
			)
			for _, group in ipairs(u.extra_groups) do
				table.insert(lines, string.format("m %s %s", name, group))
			end
		end
		ctx.file("/etc/sysusers.d/distro.conf", table.concat(lines, "\n") .. "\n")
	end,
}
