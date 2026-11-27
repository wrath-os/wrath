# Output as data

`wrath status --json` and `wrath plan --json` print JSON for scripts, monitoring and other tools.
Each has a `format` number. Fields can be added within a format; a field that's renamed,
removed or changes meaning comes with a new number, so check `format` and ignore fields you
don't know.

## `status --json`

```json
{
  "format": 1,
  "generation": 12,
  "boots_next": 12,
  "boot_failed": null,
  "from": { "url": "github:you/machines//laptop", "rev": "3f2a9c1", "date": "2026-11-26" },
  "trying": false,
  "running": "8f3c...",
  "upgrade": { "at": "2026-11-27 03:12:09", "kind": "done", "message": "up to date with ..." },
  "model": "8f3c...",
  "matches": true,
  "locked": true,
  "replaced_home_files": []
}
```

| Field | What it says |
|---|---|
| `generation` | The generation that runs, or null on a machine with none yet |
| `boots_next` | The generation the next boot starts |
| `boot_failed` | A generation that didn't boot (below), or null |
| `from` | Where its configuration came from: `url`, `rev` and `date`, or `dir` for a checkout |
| `trying` | Whether a system is being tried with `wrath try` |
| `running` | The running system's model hash |
| `upgrade` | The last automatic upgrade: `at`, `kind` (`done`, `waiting`, `failed`) and `message` |
| `model` | The configuration's model hash |
| `matches` | Whether the running system is the configuration |
| `locked` | Whether the lock pins every source |
| `replaced_home_files` | Files in homes that wrath replaced, as notes |

When the configuration can't be read, `status --json` still prints what it knows about the
machine, with `model`, `matches` and `locked` null, and exits with 1. On a machine that upgrades
from somewhere on its own, `from` is the current generation's and there's a `follows` field in
place of `model`, `matches`, `locked` and `replaced_home_files`.

`boot_failed` is there when the boot loader gave up on a generation and started an older one:

| Field | What it says |
|---|---|
| `generation` | The generation that didn't boot |
| `tries` | How many times the boot loader tried it |
| `runs` | The generation that runs instead |
| `at` | When that one was made current, or null while the failed one still is |

While `at` is null, `wrath rollback RUNS` makes the generation that runs the current one, and
keeps the record until a newer generation is made or the failed one is rolled back to.

## `plan --json`

```json
{
  "format": 1,
  "model": "876e...",
  "against": "42ac...",
  "tier": "live",
  "changes": [
    {
      "section": "units",
      "kind": "changed",
      "name": "nginx-main.service",
      "details": ["restarts: /etc/nginx/main.conf changed"],
      "tier": "live"
    }
  ]
}
```

- `model`: the model hash of the configuration being planned.
- `against`: the model hash it's compared with, usually the running system's.
- `tier`: when the whole plan takes full effect, `live`, `next-login`, `soft-reboot` or `reboot`.
  It's absent when nothing changes.
- `changes`: each change, with its `section` (`options`, `packages`, `files`, `secrets`, `units`,
  `base`), `kind` (`added`, `removed`, `changed`), `name`, `details`, and `tier` when it has one.
