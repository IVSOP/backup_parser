# backup_parser

Back up JSON/JSONC path manifests with restic.

```json
{"include": ["A/"], "exclude": ["A/B/"]}
```

`exclude` is optional and defaults to an empty array. Exclusions win across
manifests. Relative paths use `--base-path` (default: `$HOME`).
Repository and manifest defaults are `./repo` and `./files`; `example.jsonc` is ignored.

```sh
nix profile add path:.                # includes restic and gdu
cp files/example.jsonc files/personal.jsonc  # edit your paths
backup_parser init
backup_parser backup --dry-run
backup_parser backup
backup_parser list
backup_parser restore latest --target ./restored
backup_parser prune --dry-run
backup_parser prune
```

`list` (alias `snapshots`) shows every snapshot's number, full ID, date, host, and
paths. Use `list --json` for metadata as JSON, including each snapshot's number.

`restore`, `size`, `delete`, and `backup` accept a snapshot number from `list`, a
full ID, a unique ID prefix, or `latest`. Numbers start at 1, oldest first, and
can change when the repository's snapshots change; use IDs for stable references.
All-digit selectors are numbers; use `id:1234` to select an all-digit ID prefix.

```sh
backup_parser restore 2 --target ./restored
backup_parser size 2
backup_parser backup 2                 # use snapshot 2 as the parent of a new backup
backup_parser backup --parent latest   # same parent selection, using a named option
backup_parser delete 1 2 --dry-run
backup_parser delete 1 2
backup_parser delete latest --prune
```

`restore` and `size` default to `latest`. `backup` always backs up the current
manifest paths into a new snapshot; selecting a parent lets restic reuse its
unchanged file metadata. Omit the selector to let restic choose its usual parent.
Restore subtree selectors such as `2:/home/old/Documents` also work.

`delete` (alias `forget`) requires one or more explicit selectors. All selectors
are resolved before deletion, and `--dry-run` previews the operation. Deleting
snapshots removes their references; use `prune` or `delete --prune` to reclaim
unreferenced storage. Pruning preserves all remaining snapshots.

Also: `size`, `sort`, and `--help`. Omit `--target` to restore home paths into the
base and external paths in place. Restic handles passwords normally.

Develop: `nix develop path:.`. Check: `nix flake check path:.`.
