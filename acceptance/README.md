# Acceptance scenarios

Each milestone is defined by the scenarios in its directory. A milestone is done when all of its
scenarios pass. Run them with:

```sh
distro status                 # every milestone
distro status --milestone m2  # one milestone
```

## Ownership

These files are owned by a human. Changing them needs human review, including the expected
output, budgets and timeouts. Making a failing scenario pass by weakening it doesn't count as
progress.

The commands and flags in scenarios for later milestones are proposals for the interface. When a
milestone starts, they get reviewed and adjusted before the work begins, not after.

## Format

`milestones.toml` lists the milestones in order. Each milestone has a directory with one TOML file
per scenario, and the file name is the scenario ID. Every scenario has a `title`, optional `notes`
and exactly one runner table.

`[command]` runs a program in the repository root. A program named `distro` is the binary that
runs `status`.

```toml
title = "plan shows the changed value"

[command]
run = ["distro", "plan", "--config", "fixtures/configs/basic"]
exit = 0                      # expected exit code, default 0
stdout_contains = ["8081"]
stderr_contains = []
timeout_secs = 60             # default 60
```

`[manual]` passes once a human has signed it off by adding
`signoff/<milestone>-<scenario>.toml`, for example `signoff/m4-daily-driver.toml`, with who signed
it off and when.

```toml
title = "The author runs the laptop on this config daily"

[manual]
confirm = "Used daily for two weeks, repair log reviewed."
```

`[vm]` scenarios boot a test image under QEMU and run checks through the guest agent. `[vm]`
scenarios with a `config` key and `[nspawn]` scenarios show as `TODO` until a later milestone can
run them.
