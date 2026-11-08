# PAM stacks other modules share.

# pam_systemd lives with systemd rather than with PAM's own modules.
SYSTEMD = "/run/current-system/sw/lib/security/pam_systemd.so"

def unix_stacks():
    return {
        "account": [
            {"name": "unix", "control": "required", "module": "pam_unix.so"},
        ],
        "auth": [
            {"name": "env", "control": "required", "module": "pam_env.so"},
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
            {"name": "unix", "control": "sufficient", "module": "pam_unix.so", "args": ["nullok", "yescrypt"]},
        ],
        "session": [
            {"name": "env", "control": "required", "module": "pam_env.so"},
            {"name": "unix", "control": "required", "module": "pam_unix.so", "after": "env"},
            {"name": "systemd", "control": "optional", "module": SYSTEMD, "after": "unix"},
        ],
    }
