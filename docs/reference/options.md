<!-- Made by `wrath dev docs` from the code. Don't edit it by hand. -->

# Options

## A machine's configuration

### base

- `audio.enable`: a boolean, default false. Run PipeWire for users.
- `boot.kernel_params`: a list of items that are each a string, default [].
- `boot.loader`: one of "systemd-boot", default "systemd-boot".
- `desktop.autologin`: a string, optional. Start the session for this user at boot without a login.
- `desktop.session`: one of "niri", optional. The graphical session greetd starts.
- `desktop.shell`: one of "noctalia", optional. The desktop shell around the compositor: its bar, launcher, notifications and lock screen.
- `networking.dhcp`: a boolean, default true. Configure every wired interface with DHCP.
- `networking.resolved`: a boolean, default true. Run systemd-resolved for DNS, which `/etc/resolv.conf` points at.

### base-system

- `journal.persistent`: a boolean, default true. Keep the journal on disk, across boots, rather than in memory.
- `system.id`: a string, default "wrath". The system's id in `/etc/os-release`, lowercase, such as `acme`.
- `system.keymap`: a string, optional. The keyboard layout on the console, such as `sv-latin1`.
- `system.locale`: a string, default "C.UTF-8". The machine's language and formats, such as `en_US.UTF-8`.
- `system.name`: a string, default "wrath". What the system calls itself, in the boot menu and `/etc/os-release`.

### bluetooth

- `bluetooth.enable`: a boolean, default false. Run BlueZ for the machine's Bluetooth adapters.

### builds

- `builds.<name>.options`: any value, default {}. Values for the options the package's recipe declares, such as `enableTiny`.
- `builds.<name>.package`: a string, optional. The package to start from. Without one, the package with the build's name.
- `builds.<name>.patches`: a list of items that are each a string, default []. Patch files to apply, relative to the configuration.

### disks

- `disks.<name>.devices.at_least`: a string, optional. The smallest size, such as `500G`.
- `disks.<name>.devices.count`: an integer, default 1. How many such disks the machine has.
- `disks.<name>.devices.kind`: one of "nvme", "sata", "virtio", "any", default "any".
- `disks.<name>.devices.paths`: a list of items that are each a string, default []. The disks themselves, such as `/dev/disk/by-id/...`, for a machine that isn't a role. Instead of `count`, `kind` and `at_least`.
- `disks.<name>.partitions.<name>.encrypt`: a boolean, default false. Encrypted with LUKS.
- `disks.<name>.partitions.<name>.format`: one of "vfat", "ext4", "xfs", "btrfs", "swap", optional. A filesystem on just this partition. One on several is under `filesystems`.
- `disks.<name>.partitions.<name>.mount`: an absolute path or one starting with `~/`, optional.
- `disks.<name>.partitions.<name>.on`: one of "all", "first", default "all". Whether every one of the disks gets the partition, or only the first.
- `disks.<name>.partitions.<name>.options`: a list of items that are each a string, default [].
- `disks.<name>.partitions.<name>.size`: a string, required. Such as `1G`, or `rest` for what's left on the disk.
- `filesystems.<name>.degraded`: one of "boot", "stop", default "boot". With a disk of a RAID missing, boot from the others and say so, or stop until someone boots it by hand.
- `filesystems.<name>.format`: one of "btrfs", default "btrfs".
- `filesystems.<name>.mount`: an absolute path or one starting with `~/`, optional.
- `filesystems.<name>.on`: a list of items that are each a string, required. Partitions as `<disk>.<partition>`, on every disk that has them.
- `filesystems.<name>.options`: a list of items that are each a string, default [].
- `filesystems.<name>.raid`: one of "single", "raid1", default "single".
- `filesystems.<name>.subvolumes.<name>.mount`: an absolute path or one starting with `~/`, optional.
- `filesystems.<name>.subvolumes.<name>.options`: a list of items that are each a string, default [].
- `image.live`: a boolean, default false. Whether this is the system an image boots: from the image's own root, without the disks, filesystems and kept state the configuration describes, which `wrath install` puts the configured system on. `wrath build --image` sets it.
- `state.path`: an absolute path or one starting with `~/`, optional. A mount to keep things in. With this, `/` is erased at every boot.
- `state.root_size`: a string, default "25%". How much memory `/` can use, as a size or a share of memory.
- `unlock.pcrs`: a list of items that are each an integer, default [7]. The TPM measurements unlocking depends on; 7 is the Secure Boot state.
- `unlock.ssh.authorized_keys`: a list of items that are each a string, default []. Public keys allowed to log in to the initrd and unlock the disks. Without any, the initrd has no SSH.
- `unlock.ssh.port`: an integer, default 2222.
- `unlock.tpm2`: a boolean, default false. Enroll the machine's TPM at install, so its disks unlock without a passphrase while the PCRs in `pcrs` stay the same.

### files

- `files.<name>.format`: one of "json", "kdl", "systemd", "toml", optional. What to write `settings` as: `systemd` is `[Section]` and `key=value` lines.
- `files.<name>.group`: a string, optional.
- `files.<name>.mode`: an integer, optional. 0644, or 0400 with secrets in the text.
- `files.<name>.owner`: a string, optional. Only for text with secrets; root.
- `files.<name>.restart`: a list of items that are each a string, default []. Services that read this file, restarted when it changes.
- `files.<name>.settings`: any value, optional. The file's contents as TOML, written out in `format`.
- `files.<name>.source`: a file in the configuration, such as `wezterm/wezterm.lua`, optional. The file whose text this is.
- `files.<name>.text`: a string, optional.
- `users.<name>.files.<name>.format`: one of "json", "kdl", "systemd", "toml", optional. What to write `settings` as: `systemd` is `[Section]` and `key=value` lines.
- `users.<name>.files.<name>.group`: a string, optional.
- `users.<name>.files.<name>.mode`: an integer, optional. 0644, or 0400 with secrets in the text.
- `users.<name>.files.<name>.owner`: a string, optional. Only for text with secrets; root.
- `users.<name>.files.<name>.settings`: any value, optional. The file's contents as TOML, written out in `format`.
- `users.<name>.files.<name>.source`: a file in the configuration, such as `wezterm/wezterm.lua`, optional. The file whose text this is.
- `users.<name>.files.<name>.text`: a string, optional.

### firefox

- `programs.firefox.policies`: any value, default {}. Firefox's enterprise policies, such as `DisableTelemetry = true` or `ExtensionSettings`, for every user. Updating itself stays off.
- `programs.firefox.settings`: any value, default {}. Firefox's preferences, as `about:config` names them, such as `"browser.startup.homepage" = "https://example.org"`, applied each time it starts. The system's are every user's, and a user's own go over them.
- `users.<name>.programs.firefox.settings`: any value, default {}. Firefox's preferences, as `about:config` names them, such as `"browser.startup.homepage" = "https://example.org"`, applied each time it starts. The system's are every user's, and a user's own go over them.

### firewall

- `networking.firewall.allow_ping`: a boolean, default true. Answer pings, over IPv4 and IPv6.
- `networking.firewall.allowed_tcp_ports`: a list of items that are each an integer from 1 to 65535, default [].
- `networking.firewall.allowed_udp_ports`: a list of items that are each an integer from 1 to 65535, default [].
- `networking.firewall.enable`: a boolean, default true. Whether it's on.
- `networking.firewall.trusted_interfaces`: a list of items that are each a string, default []. Interfaces whose traffic is always accepted, such as a VPN's.

### fonts

- `fonts.emoji`: a string, default "Noto Color Emoji". The face programs get when they ask for `emoji`, and for emoji in any text. Its package goes in `packages`.
- `fonts.monospace`: a string, default "JetBrainsMono Nerd Font". The face programs get when they ask for `monospace`, for terminals and code. Its package goes in `packages`.
- `fonts.sans`: a string, default "Inter". The face programs get when they ask for `sans-serif`, which most text is. Its package goes in `packages`.
- `fonts.serif`: a string, default "DejaVu Serif". The face programs get when they ask for `serif`. Its package goes in `packages`.

### foreign

- `programs.foreign.enable`: a boolean, default false. Run dynamically linked programs built for other distributions.
- `programs.foreign.libraries`: a list of items that are each a string, default []. Packages whose libraries they find, beside glibc's, the C++ runtime, zlib, OpenSSL, curl and, with a desktop, its graphics and sound.

### fprintd

- `services.fprintd.enable`: a boolean, default false.

### ghostty

- `programs.ghostty.settings`: any value, default {}. Ghostty's settings, such as `font-size = 12` or `keybind = ["ctrl+t=new_tab"]`. The system's are every user's, and a user's own go over them.
- `users.<name>.programs.ghostty.settings`: any value, default {}. Ghostty's settings, such as `font-size = 12` or `keybind = ["ctrl+t=new_tab"]`. The system's are every user's, and a user's own go over them.

### git

- `users.<name>.programs.git.attributes`: a list of items that are each a string, default []. Attributes for every repository, such as `* merge=mergiraf`.
- `users.<name>.programs.git.delta`: a boolean, default false. Show diffs with delta. Its own options go in `settings.delta`.
- `users.<name>.programs.git.email`: a string, optional.
- `users.<name>.programs.git.enable`: a boolean, default false.
- `users.<name>.programs.git.ignores`: a list of items that are each a string, default []. Patterns every repository ignores.
- `users.<name>.programs.git.includes`: a list of items that are each a table, default [].
- `users.<name>.programs.git.lfs`: a boolean, default false. Large files with Git LFS.
- `users.<name>.programs.git.name`: a string, optional.
- `users.<name>.programs.git.settings`: any value, default {}. Anything else, as `section.key` or `section.subsection.key`.
- `users.<name>.programs.git.signing_key`: an absolute path or one starting with `~/`, optional. An SSH private key, or a secret, that signs commits and tags.

### jujutsu

- `users.<name>.programs.jujutsu.email`: a string, optional.
- `users.<name>.programs.jujutsu.enable`: a boolean, default false.
- `users.<name>.programs.jujutsu.name`: a string, optional.
- `users.<name>.programs.jujutsu.settings`: any value, default {}. Anything else in jj's config, such as `ui.editor`.
- `users.<name>.programs.jujutsu.signing_key`: an absolute path or one starting with `~/`, optional. An SSH private key, or a secret, that signs commits.

### kernel

- `hardware.firmware`: a list of items that are each a string, default []. Packages with firmware for the kernel, such as `linux-firmware`.
- `hardware.microcode`: one of "amd", "intel", optional. Update the CPU's microcode early in boot, for this make of CPU.
- `kernel.blacklist`: a list of items that are each a string, default []. Modules never loaded on their own.
- `kernel.initrd_modules`: a list of items that are each a string, default []. Loaded early, for what's needed to reach the disks.
- `kernel.module_options`: a table whose values are each a string, default {}. Options by module, as `modprobe` takes them.
- `kernel.modules`: a list of items that are each a string, default []. Loaded at every boot.
- `kernel.package`: a string, default "linux". The kernel and its modules, such as `linux-latest`.
- `kernel.sysctl`: a table whose values are each any value, default {}. Kernel settings by name, such as `vm.swappiness`.

### network

- `networking.dns.cache`: one of "yes", "no", "no-negative", optional.
- `networking.dns.dns_over_tls`: one of "yes", "no", "opportunistic", optional.
- `networking.dns.dnssec`: one of "yes", "no", "allow-downgrade", optional.
- `networking.dns.fallback`: a list of items that are each a string, default []. Servers to use when no network provides one.
- `networking.networks.<name>.accept_ra`: a boolean, optional. Accept IPv6 router advertisements.
- `networking.networks.<name>.dhcp`: one of "yes", "no", "ipv4", "ipv6", default "yes".
- `networking.networks.<name>.ignore_carrier_loss`: a string, optional. How long to keep the configuration when the link drops, such as `3s`.
- `networking.networks.<name>.ipv6_privacy`: a boolean, default false. Use temporary IPv6 addresses.
- `networking.networks.<name>.match`: a list of items that are each a string, required. Interface names to configure; globs like `enp*` work.
- `networking.networks.<name>.priority`: an integer from 0 to 99, default 50. Orders the file among others; the first file matching an interface applies.
- `networking.networks.<name>.sections.<name>`: a table whose values are each a string, required.

### nginx

- `services.nginx.<name>.enable`: a boolean, default false.
- `services.nginx.<name>.open_firewall`: a boolean, default false.
- `services.nginx.<name>.port`: an integer from 1 to 65535, default 80.
- `services.nginx.<name>.sites.<name>.domains`: a list of items that are each a string, default [].
- `services.nginx.<name>.sites.<name>.proxy`: a string, optional.
- `services.nginx.<name>.sites.<name>.root`: an absolute path or one starting with `~/`, optional.

### niri

- `programs.niri.settings`: any value, default {}. niri's settings, over its defaults, written as KDL: a table is a section and `true` a flag such as `prefer-no-csd`. `match = { app-id = "firefox" }` and a bind's `allow-when-locked` are properties, and in a rule `true` is given, as `open-floating true`. A key can carry arguments, such as `'output "eDP-1"'`, and a list of tables or lists repeats a node, such as `spawn-at-startup`.
- `users.<name>.programs.niri.settings`: any value, default {}. niri's settings, over its defaults, written as KDL: a table is a section and `true` a flag such as `prefer-no-csd`. `match = { app-id = "firefox" }` and a bind's `allow-when-locked` are properties, and in a rule `true` is given, as `open-floating true`. A key can carry arguments, such as `'output "eDP-1"'`, and a list of tables or lists repeats a node, such as `spawn-at-startup`.

### noctalia

- `programs.noctalia.settings`: any value, default {}. noctalia's settings, as in its `config.toml`, such as `bar.main.position = "top"`. The system's are every user's, and a user's own go over them.
- `users.<name>.programs.noctalia.settings`: any value, default {}. noctalia's settings, as in its `config.toml`, such as `bar.main.position = "top"`. The system's are every user's, and a user's own go over them.

### ollama

- `services.ollama.acceleration`: one of "auto", "cpu", "rocm", "vulkan", "cuda", default "auto". Which GPUs to use; `auto` picks what the system's packages support.
- `services.ollama.enable`: a boolean, default false. Whether it's on.
- `services.ollama.environment`: a table whose values are each a string, required.
- `services.ollama.host`: a string, default "127.0.0.1". Address to listen on.
- `services.ollama.open_firewall`: a boolean, default false.
- `services.ollama.port`: an integer from 1 to 65535, default 11434.
- `services.ollama.rocm_gfx`: a string, optional. The GPU model ROCm should assume, such as `11.0.0`, when it misdetects one.

### packages

- `packages`: a list of items that are each a string, default [].

### pam

- `security.pam.services.<name>.account`: a list of items that are each a table, default [].
- `security.pam.services.<name>.auth`: a list of items that are each a table, default [].
- `security.pam.services.<name>.password`: a list of items that are each a table, default [].
- `security.pam.services.<name>.session`: a list of items that are each a table, default [].

### program-settings

- `programs.alacritty.settings`: any value, optional. Written as toml to `/etc/xdg/alacritty/alacritty.toml`, where alacritty reads it.
- `programs.chromium.policies`: any value, optional. Written as json to `/etc/chromium/policies/managed/wrath.json`, where chromium reads it.
- `programs.chromium.recommended`: any value, optional. Written as json to `/etc/chromium/policies/recommended/wrath.json`, where chromium reads it.
- `users.<name>.programs.alacritty.settings`: any value, optional. Written as toml to `alacritty/alacritty.toml`, where alacritty reads it.

### secrets

- `secrets.<name>.file`: a string, optional. The encrypted file, relative to the configuration. Defaults to `secrets/<name>.age`.
- `secrets.<name>.group`: a string, optional. Defaults to the owner's group.
- `secrets.<name>.mode`: an integer, default 256.
- `secrets.<name>.owner`: a string, default "root".
- `secrets.<name>.path`: an absolute path or one starting with `~/`, optional. Another place to link the decrypted file from, such as a key in a home directory.
- `system.secret_identities`: a list of items that are each an absolute path or one starting with `~/`, default ["/etc/ssh/ssh_host_ed25519_key"]. Private keys on the machine that decrypt its secrets.
- `system.secret_recipients`: a list of items that are each a string, default []. Public keys `wrath secret` encrypts to: the machine's host key, and the keys of whoever edits its secrets.

### session

- `users.<name>.environment`: a table whose values are each a string, default {}. Variables every session of the user has. Values may refer to others as `$NAME`.

### ssh

- `services.ssh.enable`: a boolean, default false. Whether it's on.
- `services.ssh.open_firewall`: a boolean, default true.
- `services.ssh.password_authentication`: a boolean, default false. Also let people log in with a password.
- `services.ssh.port`: an integer from 1 to 65535, default 22.
- `users.<name>.authorized_keys`: a list of items that are each a string, default []. Public keys that can log in as this user over SSH.

### state

- `state.directories`: a list of items that are each a string, default []. Kept as they are.
- `state.files`: a list of items that are each a string, default [].
- `users.<name>.state.directories`: a list of items that are each a string, default [].
- `users.<name>.state.files`: a list of items that are each a string, default [].

### syncthing

- `services.syncthing.<name>.cert`: an absolute path or one starting with `~/`, optional. A file with the device's certificate, or a secret.
- `services.syncthing.<name>.config_dir`: an absolute path or one starting with `~/`, optional. Defaults to `.config/syncthing` in the data directory.
- `services.syncthing.<name>.data_dir`: an absolute path or one starting with `~/`, default "/var/lib/syncthing".
- `services.syncthing.<name>.devices.<name>.addresses`: a list of items that are each a string, default ["dynamic"].
- `services.syncthing.<name>.devices.<name>.id`: a string, required. The device ID, as Syncthing shows it.
- `services.syncthing.<name>.folders.<name>.devices`: a list of items that are each a string, default []. Names of devices to share with.
- `services.syncthing.<name>.folders.<name>.label`: a string, optional.
- `services.syncthing.<name>.folders.<name>.path`: an absolute path or one starting with `~/`, required. Where the folder is; `~/` is the instance user's home.
- `services.syncthing.<name>.folders.<name>.versioning.params`: a table whose values are each a string, required.
- `services.syncthing.<name>.folders.<name>.versioning.type`: one of "none", "simple", "staggered", "trashcan", "external", default "none".
- `services.syncthing.<name>.group`: a string, optional. Defaults to `syncthing` for the default user, else `users`.
- `services.syncthing.<name>.key`: an absolute path or one starting with `~/`, optional. A file with the device's private key, or a secret.
- `services.syncthing.<name>.open_firewall`: a boolean, default false.
- `services.syncthing.<name>.port`: an integer from 1 to 65535, default 8384. Port of the web interface on localhost.
- `services.syncthing.<name>.user`: a string, default "syncthing".

### system

- `system.hostname`: a string, optional. The machine's hostname. Without one, the machine's name.
- `system.timezone`: a string, default "UTC". The time zone, as tzdata names it, such as `Europe/Stockholm`.

### tailscale

- `services.tailscale.enable`: a boolean, default false. Whether it's on.
- `services.tailscale.flags`: a list of items that are each a string, default []. More flags for tailscaled.
- `services.tailscale.interface`: a string, default "tailscale0".
- `services.tailscale.open_firewall`: a boolean, default false. Open the UDP port.
- `services.tailscale.port`: an integer from 1 to 65535, default 41641. UDP port for direct connections.
- `services.tailscale.trust_interface`: a boolean, default false. Accept all traffic arriving over the tailnet.

### units

- `units`: a table whose values are each any value, default {}. systemd units by name, each with its sections, such as `Service.ExecStart`. A timer starts with the machine unless it has an `Install` section.

### user-services

- `users.<name>.services.<name>.autostart`: a boolean, default true. Whether it starts on its own: with the session for a home, with `wrath up` for a project. Otherwise it's started by hand.
- `users.<name>.services.<name>.command`: a list of items that are each a string, required. The program and its arguments. Programs on PATH need no path.
- `users.<name>.services.<name>.description`: a string, optional.
- `users.<name>.services.<name>.environment`: a table whose values are each a string, default {}.
- `users.<name>.services.<name>.needs`: a list of items that are each a string, default []. Other services of the same home or project, which start first.
- `users.<name>.services.<name>.ready`: a string, default "started". When it counts as started: `started`, `exit` for a task that runs once, or `tcp:PORT` once something listens there.
- `users.<name>.services.<name>.restart`: one of "on-failure", "always", "never", default "on-failure".
- `users.<name>.services.<name>.systemd`: any value, default {}. Unit settings only systemd has, by section, such as `Service.Nice`.

### users

- `users.<name>.description`: a string, optional.
- `users.<name>.extra_groups`: a list of items that are each a string, default [].
- `users.<name>.group`: a string, default "users".
- `users.<name>.home`: an absolute path or one starting with `~/`, optional.
- `users.<name>.initial_hashed_password`: a string, optional. The password hash a new account starts with. `passwd` on the machine replaces it.
- `users.<name>.linger`: a boolean, default false. Start the user's services at boot instead of at their first login. Without it, lingering turned on with `loginctl` is turned off again.
- `users.<name>.packages`: a list of items that are each a string, default [].
- `users.<name>.shell`: a string, optional. The backend's default shell when unset.
- `users.<name>.system`: a boolean, default false.
- `users.<name>.uid`: an integer, optional.

### wireless

- `networking.wireless.country`: a string, optional. The country whose radio rules apply, such as `SE`.
- `networking.wireless.enable`: a boolean, default false. Whether it's on.

### zen

- `programs.zen.policies`: any value, default {}. Zen's enterprise policies, such as `DisableTelemetry = true` or `ExtensionSettings`, for every user. Updating itself stays off.
- `programs.zen.settings`: any value, default {}. Zen's preferences, as `about:config` names them, such as `"browser.startup.homepage" = "https://example.org"`, applied each time it starts. The system's are every user's, and a user's own go over them.
- `users.<name>.programs.zen.settings`: any value, default {}. Zen's preferences, as `about:config` names them, such as `"browser.startup.homepage" = "https://example.org"`, applied each time it starts. The system's are every user's, and a user's own go over them.

### zram

- `zram.algorithm`: a string, default "zstd".
- `zram.enable`: a boolean, default false. Whether it's on.
- `zram.size`: a string, default "ram / 2". How big it is, as an expression of `ram`, in megabytes.

## A project's `wrath.toml`

### builds

- `builds.<name>.options`: any value, default {}. Values for the options the package's recipe declares, such as `enableTiny`.
- `builds.<name>.package`: a string, optional. The package to start from. Without one, the package with the build's name.
- `builds.<name>.patches`: a list of items that are each a string, default []. Patch files to apply, relative to the configuration.

### environment

- `environment`: a table whose values are each a string, default {}. Variables the project's shell has. Values may refer to others as `$NAME`.

### packages

- `packages`: a list of items that are each a string, default [].

### project-services

- `services.<name>.autostart`: a boolean, default true. Whether it starts on its own: with the session for a home, with `wrath up` for a project. Otherwise it's started by hand.
- `services.<name>.command`: a list of items that are each a string, required. The program and its arguments. Programs on PATH need no path.
- `services.<name>.description`: a string, optional.
- `services.<name>.environment`: a table whose values are each a string, default {}.
- `services.<name>.needs`: a list of items that are each a string, default []. Other services of the same home or project, which start first.
- `services.<name>.ready`: a string, default "started". When it counts as started: `started`, `exit` for a task that runs once, or `tcp:PORT` once something listens there.
- `services.<name>.restart`: one of "on-failure", "always", "never", default "on-failure".
- `services.<name>.systemd`: any value, default {}. Unit settings only systemd has, by section, such as `Service.Nice`.
