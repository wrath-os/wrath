# Swap on compressed memory.

def render(ctx: Context) -> None:
    zram = ctx.read("zram")
    ctx.service_package("zram-generator")
    ctx.file("/etc/modules-load.d/zram.conf", "zram\n")
    ctx.file("/etc/systemd/zram-generator.conf", formats.systemd({
        "zram0": {"zram-size": zram["size"], "compression-algorithm": zram["algorithm"]},
    }))

module(
    name = "zram",
    options = {
        "zram": types.record(
            size = option(
                types.str,
                default = "ram / 2",
                doc = "How big it is, as an expression of `ram`, in megabytes.",
            ),
            algorithm = option(types.str, default = "zstd"),
        ),
    },
    writes = [],
    enable = False,
    render = render,
)
