// The host API for modules written in TypeScript. Types are inferred from option declarations,
// so `Infer<typeof instance>` is the shape a module reads back.

declare global {
	/** A type in an option declaration. `T` is the value it holds. */
	interface Type<T> {
		readonly kind: string;
		readonly __value?: T;
	}

	/** An option: a type with a default, documentation and flags. */
	interface OptionOf<T> {
		readonly kind: "option";
		readonly __value?: T;
	}

	type Field = Type<unknown> | OptionOf<unknown>;

	/** The value an option or type holds. */
	type Infer<F> = F extends OptionOf<infer T> ? T : F extends Type<infer T> ? T : never;

	/** A value only known at service start, such as a secret. Modules pass it through. */
	interface RuntimeValue {
		readonly $runtime: unknown;
	}

	type Scalar = string | number | boolean;
	type Value = Scalar | RuntimeValue | Value[] | { [key: string]: Value };

	const types: {
		readonly bool: Type<boolean>;
		readonly int: Type<number>;
		readonly float: Type<number>;
		readonly str: Type<string>;
		readonly path: Type<string>;
		readonly port: Type<number>;
		readonly freeform: Type<Value>;
		/** A list where one value wins as a whole. */
		list<T>(item: Type<T>): Type<T[]>;
		/** A list merged from every definition, sorted and without duplicates. */
		set<T>(item: Type<T>): Type<T[]>;
		/** A list of named items placed with `before` and `after` anchors. */
		ordered<T>(item: Type<T>): Type<T[]>;
		/** Named entries chosen by the configuration. */
		map<F extends Field>(item: F): Type<Record<string, Infer<F>>>;
		/** A table with these fields. */
		record<R extends Record<string, Field>>(fields: R): Type<{ [K in keyof R]: Infer<R[K]> }>;
		one_of<const C extends readonly string[]>(choices: C): Type<C[number]>;
		range(min: number, max: number): Type<number>;
	};

	interface OptionSettings<T> {
		default?: T;
		doc?: string;
		runtime?: boolean;
	}

	function option<T>(type: Type<T>, settings: OptionSettings<T> & { optional: true }): OptionOf<T | undefined>;
	function option<T>(type: Type<T>, settings?: OptionSettings<T>): OptionOf<T>;

	type UnitValue = Scalar | Scalar[];
	type Sections = Record<string, Record<string, UnitValue>>;

	interface Ctx {
		/** The value of an option, or an object of everything under a table of options. */
		read<T = unknown>(path: string): T;
		/** Sets an option. */
		set(path: string, value: Value): void;
		/** Sets an option at the default level, so configuration overrides it. */
		default(path: string, value: Value): void;
		/** Removes items from a set or ordered list. */
		remove(path: string, items: Value[]): void;
		/** Renders a systemd unit. */
		unit(name: string, sections: Sections): void;
		/** Renders a file. */
		file(path: string, text: string, mode?: number): void;
		/** Adds a package to the system. */
		package(name: string): void;
	}

	interface ModuleDefinition {
		name: string;
		options: Record<string, Field>;
		writes: string[];
		render(ctx: Ctx): void;
	}

	/** Declares the module. A module file's default export is the result. */
	function module(definition: ModuleDefinition): ModuleDefinition;

	const formats: {
		/** Pretty-printed JSON with sorted keys. */
		json(value: Value): string;
	};
}

export {};
