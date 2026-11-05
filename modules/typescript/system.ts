// The machine's identity: hostname and timezone.

const hostname = option(types.str, { doc: "The machine's hostname." });
const timezone = option(types.str, { default: "UTC" });

export default module({
	name: "system",
	options: { "system.hostname": hostname, "system.timezone": timezone },
	writes: [],
	render(ctx) {
		const system = ctx.read<{ hostname: string; timezone: string }>("system");
		ctx.file("/etc/hostname", `${system.hostname}\n`);
		ctx.file("/etc/timezone", `${system.timezone}\n`);
	},
});
