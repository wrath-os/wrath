# Ollama, a server for local language models, sandboxed. Models
# are state, kept under /var/lib/ollama and pulled with `ollama pull`.

PACKAGES = {
    "auto": "ollama",
    "cpu": "ollama-cpu",
    "rocm": "ollama-rocm",
    "vulkan": "ollama-vulkan",
    "cuda": "ollama-cuda",
}

HOME = "/var/lib/ollama"

# Character devices GPUs need: CUDA, then ROCm.
GPU_DEVICES = [
    "char-nvidiactl",
    "char-nvidia-caps",
    "char-nvidia-frontend",
    "char-nvidia-uvm",
    "char-drm",
    "char-fb",
    "char-kfd",
]

def render(ctx: Context) -> None:
    cfg = ctx.read("services.ollama")
    ctx.package(PACKAGES[cfg["acceleration"]])

    environment = {
        "HOME": HOME,
        "OLLAMA_MODELS": HOME + "/models",
        "OLLAMA_HOST": "%s:%d" % (cfg["host"], cfg["port"]),
    }
    if "rocm_gfx" in cfg:
        environment["HSA_OVERRIDE_GFX_VERSION"] = cfg["rocm_gfx"]
    environment.update(cfg["environment"])

    ctx.unit("ollama.service", {
        "Unit": {"Description": "Server for local large language models", "After": "network.target"},
        "Service": {
            "Type": "exec",
            "ExecStart": "ollama serve",
            "Environment": ["\"%s=%s\"" % (key, value) for key, value in environment.items()],
            "DynamicUser": True,
            "StateDirectory": "ollama",
            "WorkingDirectory": HOME,
            # GPUs show up as render nodes and character devices.
            "SupplementaryGroups": "render",
            "PrivateDevices": False,
            "DevicePolicy": "closed",
            "DeviceAllow": GPU_DEVICES,
            "CapabilityBoundingSet": "",
            "LockPersonality": True,
            "MemoryDenyWriteExecute": True,
            "NoNewPrivileges": True,
            "PrivateTmp": True,
            "PrivateUsers": True,
            # Reads /proc/meminfo to size models.
            "ProcSubset": "all",
            "ProtectClock": True,
            "ProtectControlGroups": True,
            "ProtectHome": True,
            "ProtectHostname": True,
            "ProtectKernelLogs": True,
            "ProtectKernelModules": True,
            "ProtectKernelTunables": True,
            "ProtectProc": "invisible",
            "ProtectSystem": "strict",
            "RemoveIPC": True,
            "RestrictNamespaces": True,
            "RestrictRealtime": True,
            "RestrictSUIDSGID": True,
            "RestrictAddressFamilies": "AF_INET AF_INET6 AF_UNIX",
            "SystemCallArchitectures": "native",
            "SystemCallFilter": ["@system-service @resources", "~@privileged"],
            "UMask": "0077",
        },
        "Install": {"WantedBy": "multi-user.target"},
    })
    if cfg["open_firewall"]:
        ctx.set("networking.firewall.allowed_tcp_ports", [cfg["port"]])

module(
    name = "ollama",
    options = {
        "services.ollama": types.record(
            acceleration = option(
                types.one_of(PACKAGES.keys()),
                default = "auto",
                doc = "Which GPUs to use; `auto` picks what the system's packages support.",
            ),
            host = option(types.str, default = "127.0.0.1", doc = "Address to listen on."),
            port = option(types.port, default = 11434),
            open_firewall = option(types.bool, default = False),
            rocm_gfx = option(
                types.str,
                optional = True,
                doc = "The GPU model ROCm should assume, such as `11.0.0`, when it misdetects one.",
            ),
            environment = types.map(types.str),
        ),
    },
    writes = ["networking.firewall.allowed_tcp_ports"],
    enable = False,
    render = render,
)
