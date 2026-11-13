# Programs a configuration writes settings for without a module of their own (decision 0027): for
# each, the files it reads, by the option they're set under. `file` is where a user's goes, under
# ~/.config. `system`, when the program reads one, is where the system-wide one goes; a program
# without it takes settings only for users, and one with no `file` only for the system. A program
# with a module of its own, such as git, isn't here: the module owns its files.
#
# A format is one `formats` writes: "toml" or "json".

SETTINGS = {
    "alacritty": {
        "settings": {
            "file": "alacritty/alacritty.toml",
            "system": "/etc/xdg/alacritty/alacritty.toml",
            "format": "toml",
        },
    },
    # Chromium's enterprise policies: `policies` it enforces, `recommended` it starts from and
    # lets each user change. It has no file of settings a user could own.
    "chromium": {
        "policies": {
            "system": "/etc/chromium/policies/managed/wrath.json",
            "format": "json",
        },
        "recommended": {
            "system": "/etc/chromium/policies/recommended/wrath.json",
            "format": "json",
        },
    },
}
