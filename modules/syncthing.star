# Syncthing instances. Each runs as its own unit. Syncthing keeps its configuration in a file
# it rewrites, so devices and folders go in through its API: an init unit pushes the desired
# list after Syncthing starts and removes devices and folders that aren't listed, and runs
# again when the list changes.

SYNC_PORT = 22000
DISCOVERY_PORT = 21027

# Pushes devices and folders into a running Syncthing and removes the ones not listed. The
# machine's own device stays. Usage: apply.sh CONFIG_DIR GUI_ADDRESS DESIRED_JSON
APPLY = """#!/bin/sh
set -eu
config=$1/config.xml
base=http://$2/rest
desired=$3

until key=$(sed -n 's:.*<apikey>\\(.*\\)</apikey>.*:\\1:p' "$config" 2>/dev/null) &&
    [ -n "$key" ]
do
    sleep 1
done
api() {
    curl -sSf -H "X-API-Key: $key" --retry 60 --retry-delay 1 --retry-connrefused "$@"
}

me=$(api "$base/system/status" | jq -r .myID)
for kind in devices folders; do
    if [ "$kind" = devices ]; then id=deviceID; else id=id; fi
    jq -c ".$kind[]" "$desired" | while read -r item; do
        printf '%s' "$item" | api -X POST -H 'Content-Type: application/json' \\
            --data-binary @- "$base/config/$kind" >/dev/null
    done
    api "$base/config/$kind" \\
        | jq -r --slurpfile want "$desired" --arg me "$me" --arg kind "$kind" --arg id "$id" \\
            '[.[][$id]] - [$want[0][$kind][][$id]] - [$me] | .[] | @uri' \\
        | while read -r stale; do
            api -X DELETE "$base/config/$kind/$stale" >/dev/null
        done
done
"""

VERSIONING = ["none", "simple", "staggered", "trashcan", "external"]

DROPPED_CAPABILITIES = [
    "CAP_SYS_PTRACE",
    "CAP_SYS_ADMIN",
    "CAP_SETGID",
    "CAP_SETUID",
    "CAP_SETPCAP",
    "CAP_SYS_TIME",
    "CAP_KILL",
]

device = types.record(
    id = option(types.str, doc = "The device ID, as Syncthing shows it."),
    addresses = option(types.list(types.str), default = ["dynamic"]),
)

folder = types.record(
    path = option(types.path, doc = "Where the folder is; `~/` is the instance user's home."),
    label = option(types.str, optional = True),
    devices = option(types.set(types.str), doc = "Names of devices to share with."),
    versioning = types.record(
        type = option(types.one_of(VERSIONING), default = "none"),
        params = types.map(types.str),
    ),
)

instance = types.record(
    user = option(types.str, default = "syncthing"),
    group = option(
        types.str,
        optional = True,
        doc = "Defaults to `syncthing` for the default user, else `users`.",
    ),
    port = option(types.port, default = 8384, doc = "Port of the web interface on localhost."),
    data_dir = option(types.path, default = "/var/lib/syncthing"),
    config_dir = option(
        types.path,
        optional = True,
        doc = "Defaults to `.config/syncthing` in the data directory.",
    ),
    cert = option(
        types.path,
        optional = True,
        runtime = True,
        doc = "A file with the device's certificate, or a secret.",
    ),
    key = option(
        types.path,
        optional = True,
        runtime = True,
        doc = "A file with the device's private key, or a secret.",
    ),
    open_firewall = option(types.bool, default = False),
    devices = types.map(device),
    folders = types.map(folder),
)

def desired(
    ctx: Context,
    name: str,
    cfg: dict[str, typing.Any],
) -> dict[str, list[dict[str, typing.Any]]]:
    devices = [
        {"deviceID": d["id"], "name": device_name, "addresses": d["addresses"]}
        for device_name, d in cfg["devices"].items()
    ]
    folders = []
    for folder_id, f in cfg["folders"].items():
        for device_name in f["devices"]:
            if device_name not in cfg["devices"]:
                ctx.error(
                    ["services", "syncthing", name, "folders", folder_id, "devices"],
                    "there is no device `%s`" % device_name,
                )
        versioning = f["versioning"]
        folders.append({
            "id": folder_id,
            "label": f.get("label", folder_id),
            "path": f["path"],
            "devices": [{"deviceID": cfg["devices"][d]["id"]} for d in f["devices"]],
            "versioning": {
                "type": "" if versioning["type"] == "none" else versioning["type"],
                "params": versioning["params"],
            },
        })
    return {"devices": devices, "folders": folders}

def render(ctx: Context) -> None:
    open = False
    for name, cfg in ctx.read("services.syncthing").items():
        user = cfg["user"]
        group = cfg.get("group", "syncthing" if user == "syncthing" else "users")
        config_dir = cfg.get("config_dir", cfg["data_dir"] + "/.config/syncthing")
        address = "127.0.0.1:%d" % cfg["port"]
        unit = "syncthing-%s.service" % name
        serve = "syncthing serve --no-browser --gui-address=%s --config=%s --data=%s" % (
            address,
            config_dir,
            config_dir,
        )
        ctx.package("syncthing")
        ctx.package("curl")
        ctx.package("jq")

        # The device's identity, when it's kept outside Syncthing's own directory.
        owner = "-o %s -g %s" % (user, group)
        copy_keys = ["+install -dm700 %s %s" % (owner, config_dir)]
        if "cert" in cfg:
            copy_keys.append("+install -Dm644 %s %s %s/cert.pem" % (owner, cfg["cert"], config_dir))
        if "key" in cfg:
            copy_keys.append("+install -Dm600 %s %s %s/key.pem" % (owner, cfg["key"], config_dir))

        ctx.unit(unit, {
            "Unit": {"Description": "Syncthing (%s)" % name, "After": "network.target"},
            "Service": {
                "User": user,
                "Group": group,
                "Environment": ["STNORESTART=yes", "STNOUPGRADE=yes"],
                "ExecStartPre": copy_keys,
                "ExecStart": serve,
                "Restart": "on-failure",
                "SuccessExitStatus": "3 4",
                "RestartForceExitStatus": "3 4",
                "MemoryDenyWriteExecute": True,
                "NoNewPrivileges": True,
                "PrivateDevices": True,
                "PrivateMounts": True,
                "PrivateTmp": True,
                "PrivateUsers": True,
                "ProtectControlGroups": True,
                "ProtectHostname": True,
                "ProtectKernelModules": True,
                "ProtectKernelTunables": True,
                "RestrictNamespaces": True,
                "RestrictRealtime": True,
                "RestrictSUIDSGID": True,
                "CapabilityBoundingSet": "~" + " ".join(DROPPED_CAPABILITIES),
            },
            "Install": {"WantedBy": "multi-user.target"},
        })

        apply = "/etc/syncthing/%s/apply.sh" % name
        wanted = "/etc/syncthing/%s/desired.json" % name
        ctx.file(apply, APPLY, 0o755)
        ctx.file(wanted, formats.json(desired(ctx, name, cfg)))
        ctx.unit("syncthing-%s-init.service" % name, {
            "Unit": {
                "Description": "Syncthing (%s) devices and folders" % name,
                "Requires": unit,
                "After": unit,
                "PartOf": unit,
            },
            "Service": {
                "Type": "oneshot",
                "RemainAfterExit": True,
                "User": user,
                "ExecStart": "/bin/sh %s %s %s %s" % (apply, config_dir, address, wanted),
            },
            "Install": {"WantedBy": unit},
        }, restart_on = [apply, wanted])

        if user == "syncthing":
            ctx.default("users.syncthing", {
                "system": True,
                "group": group,
                "home": cfg["data_dir"],
                "description": "Syncthing service",
            })
        open = open or cfg["open_firewall"]
    if open:
        ctx.set("networking.firewall.allowed_tcp_ports", [SYNC_PORT])
        ctx.set("networking.firewall.allowed_udp_ports", [SYNC_PORT, DISCOVERY_PORT])

module(
    name = "syncthing",
    options = {"services.syncthing": types.map(instance)},
    writes = [
        "users",
        "networking.firewall.allowed_tcp_ports",
        "networking.firewall.allowed_udp_ports",
    ],
    render = render,
)
