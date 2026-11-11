# Wifi with iwd. `iwctl` joins networks, and iwd remembers them in /var/lib/iwd. networkd gives
# wireless interfaces their addresses, as it does wired ones.

NETWORK = "/etc/systemd/network/80-wireless.network"

def render(ctx: Context) -> None:
    cfg = ctx.read("networking.wireless")
    ctx.package("iwd")
    ctx.service_package("iwd")
    ctx.enable("iwd.service")
    # The radio rules for each country, which the kernel loads before it lets a radio send.
    ctx.set("hardware.firmware", ["wireless-regdb"])
    if "country" in cfg:
        ctx.set("boot.kernel_params", ["cfg80211.ieee80211_regdom=" + cfg["country"]])
    ctx.file("/etc/iwd/main.conf", formats.systemd({
        "General": {"EnableNetworkConfiguration": False},
        "Network": {"NameResolvingService": "systemd"},
    }))
    ctx.restart("iwd.service", on = ["/etc/iwd/main.conf"])
    ctx.file(NETWORK, formats.systemd({
        "Match": {"Type": "wlan"},
        "Network": {"DHCP": "yes", "IgnoreCarrierLoss": "3s"},
        # Wifi joins networks when there are any, and doesn't hold up booting when there aren't.
        "Link": {"RequiredForOnline": False},
    }))
    ctx.reload("systemd-networkd.service", on = [NETWORK])

module(
    name = "wireless",
    options = {
        "networking.wireless": types.record(
            country = option(
                types.str,
                optional = True,
                doc = "The country whose radio rules apply, such as `SE`.",
            ),
        ),
    },
    writes = ["hardware.firmware", "boot.kernel_params"],
    enable = False,
    render = render,
)
