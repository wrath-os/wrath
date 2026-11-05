// The default PAM stacks for local logins and sudo.

function unixStacks(): Value {
	return {
		account: [{ name: "unix", control: "required", module: "pam_unix.so" }],
		auth: [
			{ name: "env", control: "required", module: "pam_env.so" },
			{
				name: "unix",
				control: "sufficient",
				module: "pam_unix.so",
				args: ["likeauth", "try_first_pass"],
				after: "env",
			},
			{ name: "deny", control: "required", module: "pam_deny.so", after: "unix" },
		],
		password: [
			{ name: "unix", control: "sufficient", module: "pam_unix.so", args: ["nullok", "yescrypt"] },
		],
		session: [
			{ name: "env", control: "required", module: "pam_env.so" },
			{ name: "unix", control: "required", module: "pam_unix.so", after: "env" },
			{ name: "systemd", control: "optional", module: "pam_systemd.so", after: "unix" },
		],
	};
}

export default module({
	name: "pam-base",
	options: {},
	writes: ["security.pam.services.login", "security.pam.services.sudo"],
	render(ctx) {
		for (const service of ["login", "sudo"]) {
			ctx.set(`security.pam.services.${service}`, unixStacks());
		}
	},
});
