-- Fingerprint authentication: the fprintd service and a PAM rule tried before the password.

local SERVICES = { "login", "sudo" }

return {
	name = "fprintd",
	options = {
		["services.fprintd.enable"] = option(types.bool, { default = false }),
	},
	writes = { "security.pam.services.login.auth", "security.pam.services.sudo.auth" },
	render = function(ctx)
		if not ctx.read("services.fprintd.enable") then
			return
		end
		ctx.package("fprintd")
		ctx.unit("fprintd.service", {
			Unit = { Description = "Fingerprint authentication daemon" },
			Service = { Type = "dbus", BusName = "net.reactivated.Fprint", ExecStart = "fprintd" },
		})
		for _, service in ipairs(SERVICES) do
			ctx.set("security.pam.services." .. service .. ".auth", {
				{
					name = "fprintd",
					control = "sufficient",
					module = "pam_fprintd.so",
					after = "env",
					before = "unix",
				},
			})
		end
	end,
}
