// Users and groups, rendered as a sysusers.d file.

const user = types.record({
	description: option(types.str, { optional: true }),
	uid: option(types.int, { optional: true }),
	group: option(types.str, { default: "users" }),
	extra_groups: option(types.set(types.str), { default: [] }),
	home: option(types.path, { optional: true }),
	shell: option(types.str, { default: "/run/current-system/sw/bin/bash" }),
	system: option(types.bool, { default: false }),
	packages: option(types.set(types.str), { default: [] }),
});

type User = Infer<typeof user>;

function homeOf(name: string, u: User): string {
	if (u.home !== undefined) return u.home;
	return u.system ? "/" : `/home/${name}`;
}

export default module({
	name: "users",
	options: { users: types.map(user) },
	writes: [],
	render(ctx) {
		const lines: string[] = [];
		for (const [name, u] of Object.entries(ctx.read<Record<string, User>>("users"))) {
			const uid = u.uid !== undefined ? String(u.uid) : "-";
			lines.push(`u ${name} ${uid} "${u.description ?? ""}" ${homeOf(name, u)} ${u.shell}`);
			for (const group of u.extra_groups) {
				lines.push(`m ${name} ${group}`);
			}
		}
		ctx.file("/etc/sysusers.d/distro.conf", lines.join("\n") + "\n");
	},
});
