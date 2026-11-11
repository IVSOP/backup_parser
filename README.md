# backup_parser

A Rust CLI for JSON/JSONC path manifests and restic. Ports the existing Python
`init`, `backup`, `restore`, `size`, and `sort` commands; cleanup is outside its
scope. Uses clap for the CLI, typed serde deserialization for manifests and restic
metadata, and jsonc-parser for parsing and comment-preserving sorting.

## Quick start

With Nix installed:

```sh
nix develop path:.
cp files/example.jsonc files/personal.jsonc
# Edit files/personal.jsonc to select your paths.
cargo run -- init
cargo run -- backup --dry-run
cargo run -- backup
```

Restic prompts for the repository password. Its usual `RESTIC_PASSWORD`,
`RESTIC_PASSWORD_FILE`, and `RESTIC_PASSWORD_COMMAND` variables also work. Never
commit passwords or put them in Nix expressions: Nix store contents are readable
by other users. `--password-file secret` resolves `secret` against the base.

Without Nix, install Rust, restic, and (for `size`) gdu, then use
`cargo install --locked --path .`. Current stable dependencies are recorded in
`Cargo.lock`; the project uses Rust edition 2024.

## Manifests

Every `.json` or `.jsonc` file directly inside `--files-dir` is read, in filename
order. `example.json` and `example.jsonc` are ignored. Both extensions accept JSONC
comments and trailing commas. Every manifest must contain exactly these two arrays:

```jsonc
{
  "include": [
    "A/",
    ".ssh/", // relative to the base
    "/etc/nixos/", // absolute paths keep their location
  ],
  "exclude": [
    "A/B/",
  ],
}
```

This backs up `A/C`, `A/D`, and everything else in `A`, except `A/B` and its entire
subtree. Exclusions apply globally across all manifests and always win, even if
another manifest explicitly includes `A/B` or a file beneath it. `A/B` does not
exclude `A/BB`, or another directory named `B` elsewhere.

Paths must be nonempty strings without NUL characters. Unknown keys, duplicate
keys, missing arrays, old top-level arrays, and invalid syntax are rejected before
restic runs. To migrate an old manifest, wrap its array as
`{"include": [/* existing entries */], "exclude": []}`. No manifests or existing
repositories in `~/backups/backup` are modified by this project.

Path rules:

- The base defaults to `$HOME`. Override it with `--base-path DIR` (aliases:
  `--base-dir`, `--home`). It must already be a directory and is canonicalized.
- Relative entries, `~/`, `$HOME/`, and `${HOME}/` resolve against that base. Bare
  `~`, `$HOME`, and `${HOME}` select the base itself. Other environment variables
  and `~otheruser` are not expanded.
- Includes and excludes support filesystem globs: `*`, `?`, `[]`, and recursive
  `**`. Existing literal names take priority, so `notes [draft].txt` selects that
  exact file when it exists. Wildcards do not select dotfiles unless that component
  starts with a literal dot. Paths are never evaluated as shell commands.
- Globs are expanded once, before restic starts. Excluded matches become literal,
  absolute restic exclusions; restic prunes those subtrees during traversal.
  Exclusions are filesystem paths/globs, not restic's unanchored filter language.
  To exclude all Cargo build directories under a tree, use e.g.
  `"Desktop/rust/**/target/"`.
- Duplicate sources are removed. Unmatched include globs and missing includes are
  passed to restic, preserving its incomplete-backup exit status. An empty source
  list after exclusions is an error.
- Spaces, `#`, newlines, and shell characters in source names are preserved using
  restic's NUL-delimited `--files-from-raw`. Manifest strings and restic filter
  paths use UTF-8. Restic handles directory traversal and symlinks itself.

`--repo` defaults to `RESTIC_REPOSITORY`, or `repo` if unset. Local relative
repository paths and `--files-dir` (default: `files`) resolve against the **caller's
working directory**, independently of the base. Restic remote repository URLs
such as `sftp:host:/backups` and `s3:https://host/bucket` pass through unchanged.

Two deliberate differences from the Python defaults make installation practical:
the working directory replaces the script directory for repository/manifests,
and `$HOME` remains the base under sudo too. To back up your home using sudo, pass
the original home explicitly:

```sh
sudo backup_parser --base-path "$HOME" --repo /backups/restic \
  backup --files-dir "$HOME/backups/manifests"
```

## Commands

Global flags can appear before or after a subcommand. `--help` is available for
each command.

```sh
backup_parser init
backup_parser backup --files-dir ./files --dry-run
backup_parser backup
backup_parser restore latest --dry-run
backup_parser restore SNAPSHOT --base-path /home/new-user --verify
backup_parser restore SNAPSHOT --target /mnt/staging
backup_parser restore SNAPSHOT /mnt/staging
backup_parser restore SNAPSHOT:/home/old-user/.ssh --target /mnt/ssh
backup_parser size latest
backup_parser sort --files-dir ./files --dry-run
backup_parser sort
```

Backup uses maximum compression and verbose output. New snapshots contain
absolute paths and the same `backup-layout:absolute-v1` / percent-encoded
`backup-base:` tags as the Python tools. Existing repositories remain usable.
Restic's exit status is propagated, including exit 3 for an incomplete backup;
configuration and local I/O errors return 1, and clap usage errors return 2.

Without an explicit target, restore remaps paths beneath the snapshot's saved
base into the selected base. Absolute paths outside the saved base are restored
to their original locations and can overwrite system files. The full plan is
validated before any restore starts; `--dry-run` passes through to restic. An
explicit target stages the stored tree beneath that directory, with no remapping;
relative targets resolve from the caller's working directory. Subfolder selectors
use normal restic semantics, defaulting to the base when no target is supplied.

Legacy snapshots are inspected for relative, absolute, or mixed layouts. Homes
under `/home/USER`, `/Users/USER`, and `/root` are inferred from recorded paths.
Use `--source-base /original/base` for other old bases. Ambiguous snapshots stop
before restoring; select `--layout absolute` or `--layout relative`, or stage with
`--target`. The resolved snapshot ID is pinned across restore jobs.

`size` exports restic's native ncdu format, opens it in `gdu -f`, and removes the
temporary export afterward. `sort` sorts both arrays by decoded string value,
preserves comments and file permissions (and ownership when run as root), and
validates every manifest before writing. Opening/footer comments stay in place;
inline comments travel with the preceding entry, and notes between entries with
the following entry. Each file is replaced atomically; the entire batch is not a
single transaction. Dry runs validate without replacing files.

## Nix and NixOS

The flake pins `nixos-unstable` in `flake.lock`. It exposes
`packages.<system>.backup_parser`, `packages.<system>.default`, a default app,
development shell, formatter, and a package check for `x86_64-linux` and
`aarch64-linux`.

```sh
nix build path:.
./result/bin/backup_parser --help
nix run path:. -- --help
nix profile add path:.
nix flake check path:.
nix develop path:. --command nixfmt flake.nix nix/package.nix
```

`path:.` also works in a fresh Git checkout with untracked project files. Once
the project files are tracked, ordinary `nix build`, `nix run`, `nix develop`, and
`nix flake check` work too; `nix fmt` formats the Nix source tree. Commit both lockfiles. Update intentionally with
`cargo update` and `nix flake update`.

For declarative NixOS installation, add this project as an input to your system
flake (replace the example path with its permanent location or a Git URL):

```nix
inputs.backup-parser.url = "path:/path/to/backup_parser";
inputs.backup-parser.inputs.nixpkgs.follows = "nixpkgs";
```

Add a module to the `modules` list of your `nixosSystem`:

```nix
({ pkgs, ... }: {
  environment.systemPackages = [
    inputs.backup-parser.packages.${pkgs.stdenv.hostPlatform.system}.default
  ];
})
```

Here `inputs` is the argument to your system flake's `outputs`. For Home Manager,
use the same package in `home.packages`. Keep repository state, password files,
and editable manifests outside the Nix store. Installation does not initialize a
repository or schedule backups.

The package uses Nixpkgs' standard
[`rustPlatform.buildRustPackage` with `cargoLock`](https://nixos.org/manual/nixpkgs/unstable/#importing-a-cargo.lock-file),
`lib.fileset` to select source files, and `makeBinaryWrapper` to bundle restic/gdu
on the executable's PATH. Tests run against real restic in the Nix build sandbox.
The derivation is also reusable via `pkgs.callPackage ./nix/package.nix { }`.

## Development

```sh
nix develop path:.
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
nix flake check path:.
```

Tests require restic. They use temporary repositories and cover nested/global
exclusions, literal/glob paths, unusual filenames, missing inputs, passwords,
dry runs, JSONC validation/sorting, tagged and legacy restores, staging, and size
exports. They do not access your backup repository or source manifests.

Restic behavior follows its [backup](https://restic.readthedocs.io/en/stable/040_backup.html)
and [restore](https://restic.readthedocs.io/en/stable/050_restore.html) documentation.
