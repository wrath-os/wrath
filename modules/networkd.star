# Networking: systemd-networkd, and DHCP on every wired interface
# unless the configuration describes its networks.

def render(ctx: Context) -> None:
    dhcp = ctx.read("networking.dhcp")
    resolved = ctx.read("networking.resolved")
    if dhcp:
        ctx.file("/etc/systemd/network/99-dhcp.network", formats.systemd({
            "Match": {"Type": "ether", "Kind": "!*"},
            "Network": {"DHCP": "yes"},
        }))
    if dhcp or ctx.read("networking.networks"):
        ctx.enable("systemd-networkd.service")
        ctx.enable("systemd-networkd-wait-online.service")
        ctx.enable("network-online.target", wanted_by = "multi-user.target")
    if resolved:
        ctx.enable("systemd-resolved.service")

module(name = "networkd", options = {}, writes = [], render = render)
