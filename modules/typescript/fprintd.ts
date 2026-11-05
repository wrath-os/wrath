// Fingerprint authentication: the fprintd service and a PAM rule tried before the password.

const SERVICES = ["login", "sudo"];

export default module({
	name: "fprintd",
	options: { "services.fprintd.enable": option(types.bool, { default: false }) },
	writes: ["security.pam.services.login.auth", "security.pam.services.sudo.auth"],
	render(ctx) {
		if (!ctx.read<boolean>("services.fprintd.enable")) return;
		ctx.package("fprintd");
		ctx.unit("fprintd.service", {
			Unit: { Description: "Fingerprint authentication daemon" },
			Service: { Type: "dbus", BusName: "net.reactivated.Fprint", ExecStart: "fprintd" },
		});
		for (const service of SERVICES) {
			ctx.set(`security.pam.services.${service}.auth`, [
				{
					name: "fprintd",
					control: "sufficient",
					module: "pam_fprintd.so",
					after: "env",
					before: "unix",
				},
			]);
		}
	},
});
