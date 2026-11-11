# backup_parser

Back up JSON/JSONC path manifests with restic.

```json
{"include": ["A/"], "exclude": ["A/B/"]}
```

Exclusions win across manifests. Relative paths use `--base-path` (default: `$HOME`).
Repository and manifest defaults are `./repo` and `./files`; `example.jsonc` is ignored.

```sh
nix profile add path:.                # includes restic and gdu
cp files/example.jsonc files/personal.jsonc  # edit your paths
backup_parser init
backup_parser backup --dry-run
backup_parser backup
backup_parser restore latest --target ./restored
```

Also: `size`, `sort`, and `--help`. Omit `--target` to restore home paths into the
base and external paths in place. Restic handles passwords normally.

Develop: `nix develop path:.`. Check: `nix flake check path:.`.
