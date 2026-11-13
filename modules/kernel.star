# The kernel, its modules and settings, and what the hardware needs loaded into it: firmware and
# CPU microcode. A machine says what it has; nothing is detected at build time.

MICROCODE_PACKAGES = {"amd": "microcode-amd", "intel": "microcode-intel"}

SYSCTL = "/etc/sysctl.d/60-kernel.conf"
MODULES = "/etc/modules-load.d/kernel.conf"

def modprobe_config(kernel: dict[str, typing.Any]) -> str:
    lines = ["blacklist " + module for module in sorted(kernel["blacklist"])]
    lines += ["options %s %s" % item for item in sorted(kernel["module_options"].items())]
    return "".join([line + "\n" for line in lines])

def render(ctx: Context) -> None:
    kernel = ctx.read("kernel")
    hardware = ctx.read("hardware")
    if kernel["sysctl"]:
        ctx.file(SYSCTL, "".join([
            "%s = %s\n" % (key, value) for key, value in sorted(kernel["sysctl"].items())
        ]))
        ctx.restart("systemd-sysctl.service", on = [SYSCTL])
    if kernel["modules"]:
        ctx.file(MODULES, lines(sorted(kernel["modules"])))
        ctx.restart("systemd-modules-load.service", on = [MODULES])
    modprobe = modprobe_config(kernel)
    if modprobe:
        ctx.file("/etc/modprobe.d/wrath.conf", modprobe)
    # Taking effect takes a boot, so these are for the backend to build into it.
    settings = {
        "package": kernel["package"],
        "initrd_modules": sorted(kernel["initrd_modules"]),
        "modprobe": modprobe,
        "firmware": sorted(hardware["firmware"]),
    }
    if "microcode" in hardware:
        settings["microcode"] = {
            "package": MICROCODE_PACKAGES[hardware["microcode"]],
            "image": hardware["microcode"] + "-ucode.img",
        }
    ctx.base("kernel", settings)

module(
    name = "kernel",
    options = {
        "kernel": types.record(
            package = option(
                types.str,
                default = "linux",
                doc = "The kernel and its modules, such as `linux-latest`.",
            ),
            modules = option(types.set(types.str), doc = "Loaded at every boot."),
            initrd_modules = option(
                types.set(types.str),
                doc = "Loaded early, for what's needed to reach the disks.",
            ),
            blacklist = option(
                types.set(types.str),
                doc = "Modules never loaded on their own.",
            ),
            module_options = option(
                types.map(types.str),
                doc = "Options by module, as `modprobe` takes them.",
            ),
            sysctl = option(
                types.map(types.freeform),
                doc = "Kernel settings by name, such as `vm.swappiness`.",
            ),
        ),
        "hardware": types.record(
            firmware = option(
                types.set(types.str),
                doc = "Packages with firmware for the kernel, such as `linux-firmware`.",
            ),
            microcode = option(
                types.one_of(["amd", "intel"]),
                optional = True,
                doc = "Update the CPU's microcode early in boot, for this make of CPU.",
            ),
        ),
    },
    writes = [],
    render = render,
)
