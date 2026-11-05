# 0002: VM harness with a guest agent over vsock

Status: accepted, 2026-11-05

## Context

System scenarios need to boot a full OS image under UEFI with a TPM, run commands in it and
check the results. The same harness has to work on a plain NixOS image now and on our own images
later, so it can't depend on how either is built internally.

## Options

1. The NixOS test driver. Mature, but tied to NixOS's own VM setup and Python test scripts.
2. Serial console scripting. Works with any image, but output parsing is fragile.
3. QEMU plus a small guest agent that the harness talks to over a socket.

## Decision

Option 3.

- `distro-agent` runs in the guest and listens on vsock port 7000. The protocol is
  newline-delimited JSON with two requests, `ping` and `run`. Everything else (waiting for
  units, checking output) is built on `run` in the harness, so the agent stays small.
- vsock needs no guest networking, works before the network is up and is reachable from
  unprivileged host processes through `/dev/vhost-vsock`.
- A test image is any flake output under `factory/nix` that contains `disk.qcow2`, a
  UEFI-bootable disk whose system starts the agent.
- Each run gets a directory under `target/distro-check/runs/` with a copy-on-write overlay of the
  disk, writable UEFI variables, the TPM state, the serial console log and an `interactive.sh`
  that boots the same VM with the console on the terminal. On failure it also gets the guest
  journal and a screenshot.
- A failure prints `distro check <scenario> --image <store path>`, which reruns against the exact
  image of the failed run. `--run-reproduction` verifies that it fails the same way.

## Consequences

- The harness depends on QEMU, OVMF and swtpm from the devenv shell, and on `/dev/kvm` and
  `/dev/vhost-vsock` on the host.
- vsock context IDs must be unique on the host. They are picked from the process ID and the clock,
  so parallel runs are unlikely to collide but not guaranteed not to.
- Test images include the agent, so any change to the agent rebuilds them. The agent's Nix build
  only sees the agent's own sources, so changes to other crates don't.
