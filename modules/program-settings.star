# Settings for programs that have no module of their own, written in their own format into the
# file they read (decision 0027). Which programs and files these are is in `lib/programs.star`.
# Setting any of a program's files adds the program, for the system or for that user.

load("lib/programs.star", "SETTINGS")

# The programs that read a system-wide file, which a system configuration sets.
SYSTEM_WIDE = {
    program: files
    for program, files in SETTINGS.items()
    if any(["system" in f for f in files.values()])
}

# The programs that read a file in each home, which a user's configuration sets.
PER_USER = {
    program: files
    for program, files in SETTINGS.items()
    if any(["file" in f for f in files.values()])
}

def render_file(format: str, settings: dict[str, typing.Any]) -> str:
    if format == "toml":
        return formats.toml(settings)
    if format == "json":
        return formats.json(settings)
    fail("there's no writer for the format `%s` yet" % format)

def options_of(program: str, files: dict[str, typing.Any], where: str) -> typing.Any:
    """The options for `program`'s files that have a place at `where`, "file" or "system"."""
    return types.record(**{
        name: option(
            types.freeform,
            optional = True,
            doc = "Written as %s to `%s`, where %s reads it." % (f["format"], f[where], program),
        )
        for name, f in files.items()
        if where in f
    })

def render(ctx: Context) -> None:
    for program, files in SETTINGS.items():
        system = ctx.read("programs." + program) if program in SYSTEM_WIDE else {}
        for name, f in files.items():
            if name in system:
                ctx.file(f["system"], render_file(f["format"], system[name]), setting = True)
                ctx.package(program)
        if program not in PER_USER:
            continue
        for user, u in ctx.read("users").items():
            own = u["programs"][program]
            for name, f in files.items():
                if name in own:
                    ctx.file("~/.config/" + f["file"], render_file(f["format"], own[name]),
                             user = user, setting = True)
                    ctx.package(program, user = user)

module(
    name = "program-settings",
    options = {
        "programs." + program: options_of(program, files, "system")
        for program, files in SYSTEM_WIDE.items()
    },
    user_options = {
        "programs." + program: options_of(program, files, "file")
        for program, files in PER_USER.items()
    },
    writes = [],
    render = render,
)
