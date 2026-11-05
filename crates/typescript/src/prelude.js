// Runs before any module. Declarations are plain frozen objects the host reads back; the shapes
// match modules/typescript/distro.d.ts. Everything that depends on time, randomness or garbage
// collection is removed.
"use strict";

const freeze = Object.freeze;
const scalar = (kind) => freeze({ kind });
const list = (merge) => (item) => freeze({ kind: "list", merge, item });

globalThis.types = freeze({
	bool: scalar("bool"),
	int: scalar("int"),
	float: scalar("float"),
	str: scalar("str"),
	path: scalar("path"),
	port: scalar("port"),
	freeform: scalar("freeform"),
	list: list("atomic"),
	set: list("set"),
	ordered: list("ordered"),
	map: (item) => freeze({ kind: "map", item }),
	record: (fields) => freeze({ kind: "record", fields: freeze({ ...fields }) }),
	one_of: (choices) => freeze({ kind: "one_of", choices: freeze([...choices]) }),
	range: (min, max) => freeze({ kind: "range", min, max }),
});

globalThis.option = (type, settings = {}) =>
	freeze({ kind: "option", type, settings: freeze({ ...settings }) });

globalThis.module = (definition) => definition;

delete globalThis.Date;
delete globalThis.WeakRef;
delete globalThis.FinalizationRegistry;
Object.defineProperty(Math, "random", {
	value: () => {
		throw new Error("Math.random is not available in modules");
	},
});
freeze(Math);
