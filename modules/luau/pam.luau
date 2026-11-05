-- PAM services, rendered as /etc/pam.d files. Other modules add rules to the stacks; rules are
-- placed with `before` and `after` anchors.

local rule = types.record({
	name = option(types.str),
	control = option(types.str),
	module = option(types.str),
	args = option(types.list(types.str), { optional = true }),
})

local function stack()
	return option(types.ordered(rule), { default = {} })
end

local KINDS = { "account", "auth", "password", "session" }

return {
	name = "pam",
	options = {
		["security.pam.services"] = types.map(types.record({
			account = stack(),
			auth = stack(),
			password = stack(),
			session = stack(),
		})),
	},
	writes = {},
	render = function(ctx)
		for service, stacks in pairs(ctx.read("security.pam.services")) do
			local lines = {}
			for _, kind in ipairs(KINDS) do
				for _, r in ipairs(stacks[kind]) do
					local line = string.format("%s %s %s", kind, r.control, r.module)
					if r.args then
						line ..= " " .. table.concat(r.args, " ")
					end
					table.insert(lines, line)
				end
			end
			ctx.file("/etc/pam.d/" .. service, table.concat(lines, "\n") .. "\n")
		end
	end,
}
