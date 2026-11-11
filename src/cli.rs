use clap::{Args, Parser, Subcommand, ValueEnum};
use std::{ffi::OsString, path::PathBuf};

#[derive(Debug, Parser)]
#[command(version, about = "Back up JSONC path manifests with restic")]
pub struct Cli {
    /// Base for relative manifest paths and restored home files (default: $HOME)
    #[arg(long, aliases = ["base-dir", "home"], env = "HOME", hide_env_values = true, global = true, value_name = "DIR")]
    pub base_path: Option<PathBuf>,
    /// Restic repository; local relative paths use the caller's working directory
    #[arg(long, env = "RESTIC_REPOSITORY", default_value = "repo", global = true)]
    pub repo: OsString,
    /// Restic password file; relative paths use the base directory
    #[arg(long, global = true, value_name = "FILE")]
    pub password_file: Option<PathBuf>,
    #[command(subcommand)]
    pub operation: Operation,
}

#[derive(Debug, Subcommand)]
pub enum Operation {
    /// Initialize the restic repository
    Init,
    /// Back up the union of includes, minus all exclusions
    Backup(ManifestArgs),
    /// Restore home paths into the base and system paths to their original locations
    Restore(RestoreArgs),
    /// Browse a snapshot's size using gdu and restic's ncdu export
    Size {
        #[arg(default_value = "latest")]
        snapshot: String,
    },
    /// Sort include/exclude arrays, preserving comments and file permissions
    Sort(ManifestArgs),
}

#[derive(Debug, Args)]
pub struct ManifestArgs {
    /// Directory containing .json and .jsonc manifests (example files are ignored)
    #[arg(long, default_value = "files", value_name = "DIR")]
    pub files_dir: PathBuf,
    /// Validate and preview without writing backups or manifests
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct RestoreArgs {
    #[arg(default_value = "latest")]
    pub snapshot: String,
    /// Stage the stored tree beneath this directory, without remapping paths
    #[arg(conflicts_with = "target")]
    pub positional_target: Option<PathBuf>,
    /// Stage the stored tree beneath this directory, without remapping paths
    #[arg(long, value_name = "DIR")]
    pub target: Option<PathBuf>,
    /// Absolute base path used to create an old snapshot
    #[arg(long, value_name = "DIR")]
    pub source_base: Option<PathBuf>,
    /// Resolve the path layout of an untagged legacy snapshot
    #[arg(long, value_enum, default_value = "auto")]
    pub layout: Layout,
    /// Ask restic to show changes without restoring files
    #[arg(long)]
    pub dry_run: bool,
    /// Verify restored content
    #[arg(long)]
    pub verify: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Layout {
    Auto,
    Absolute,
    Relative,
}
