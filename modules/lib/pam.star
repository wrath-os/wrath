# PAM stacks other modules share.

# pam_systemd lives with systemd rather than with PAM's own modules.
SYSTEMD = "/run/system/lib/security/pam_systemd.so"

# Every login gets the system's environment, including ones that run a single command.
ENV = {
    "name": "env",
    "control": "required",
    "module": "pam_env.so",
    "args": ["conffile=/etc/pam/environment", "readenv=0"],
}

def unix_stacks() -> dict[str, list[dict[str, str | list[str]]]]:
    return {
        "account": [
            {"name": "unix", "control": "required", "module": "pam_unix.so"},
        ],
        "auth": [
            ENV,
            {
                "name": "unix",
                "control": "sufficient",
                "module": "pam_unix.so",
                "args": ["likeauth", "try_first_pass"],
                "after": "env",
            },
            {"name": "deny", "control": "required", "module": "pam_deny.so", "after": "unix"},
        ],
        "password": [
            {
                "name": "unix",
                "control": "sufficient",
                "module": "pam_unix.so",
                "args": ["nullok", "yescrypt"],
            },
        ],
        "session": [
            ENV,
            {"name": "unix", "control": "required", "module": "pam_unix.so", "after": "env"},
            {"name": "systemd", "control": "optional", "module": SYSTEMD, "after": "unix"},
        ],
    }
