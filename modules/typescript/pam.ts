// PAM services, rendered as /etc/pam.d files. Other modules add rules to the stacks; rules are
// placed with `before` and `after` anchors.

const rule = types.record({
	name: option(types.str),
	control: option(types.str),
	module: option(types.str),
	args: option(types.list(types.str), { optional: true }),
});

const stack = () => option(types.ordered(rule), { default: [] });

const service = types.record({
	account: stack(),
	auth: stack(),
	password: stack(),
	session: stack(),
});

const KINDS = ["account", "auth", "password", "session"] as const;

export default module({
	name: "pam",
	options: { "security.pam.services": types.map(service) },
	writes: [],
	render(ctx) {
		const services = ctx.read<Record<string, Infer<typeof service>>>("security.pam.services");
		for (const [name, stacks] of Object.entries(services)) {
			const lines: string[] = [];
			for (const kind of KINDS) {
				for (const r of stacks[kind]) {
					const args = r.args !== undefined ? ` ${r.args.join(" ")}` : "";
					lines.push(`${kind} ${r.control} ${r.module}${args}`);
				}
			}
			ctx.file(`/etc/pam.d/${name}`, lines.join("\n") + "\n");
		}
	},
});
