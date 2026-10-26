# Seed data

What a new server's world starts with, one folder per client build. When the server starts, each
file here is loaded into its world database table if that table is empty, so a fresh clone needs
no imports: point `[data] game` at a client install and run.

| file | table | from |
|---|---|---|
| `672261/bnpc_spawn.csv` | `bnpc_spawn` | Sapphire's battle NPCs, filtered to the β build (see [NOTICE](../NOTICE.md)) |

Everything here fits its build: `crystal-tools prune` finds (and with `--apply` removes) world
content whose territory, sheet rows or layout layers the build does not have, and the importers
apply the same checks. After changing the world database, `crystal-tools export --source sapphire` writes the file back
(without what was placed by hand while testing). `[data] seed` (default `data`) points elsewhere, or unset turns seeding off.
