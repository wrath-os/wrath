# Audio for users: PipeWire, started with each user's session,
# with WirePlumber managing it and RealtimeKit giving it realtime priority. Programs that speak
# PulseAudio talk to PipeWire's PulseAudio server.

def render(ctx: Context) -> None:
    if not ctx.read("audio.enable"):
        return
    for package in ["pipewire", "wireplumber", "rtkit"]:
        ctx.service_package(package)
    ctx.enable("pipewire.socket", user = True)
    ctx.enable("pipewire-pulse.socket", user = True)
    ctx.enable("wireplumber.service", user = True)
    ctx.enable("pipewire.service", user = True, wanted_by = "default.target")

module(name = "audio", options = {}, writes = [], render = render)
