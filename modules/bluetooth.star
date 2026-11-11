# Bluetooth: BlueZ's daemon, which systemd starts once the kernel finds an adapter, and
# bluetoothctl to pair devices with. With audio on, PipeWire plays to Bluetooth headphones.

def render(ctx: Context) -> None:
    if not ctx.read("bluetooth.enable"):
        return
    ctx.service_package("bluez")
    ctx.enable("bluetooth.service")

module(
    name = "bluetooth",
    options = {
        "bluetooth.enable": option(
            types.bool,
            default = False,
            doc = "Run BlueZ for the machine's Bluetooth adapters.",
        ),
    },
    writes = [],
    render = render,
)
