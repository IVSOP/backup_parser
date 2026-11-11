mod cli;
mod manifest;
mod paths;
mod restic;
mod restore;

use anyhow::{Context, Result};
use clap::Parser;
use cli::{Cli, Operation};
use restic::Restic;
use std::process::ExitCode;

fn run(cli: Cli) -> Result<u8> {
    let base = cli
        .base_path
        .context("HOME is unset; pass --base-path DIR")?;
    let base = paths::base_directory(&base)?;
    let restic = Restic::new(&cli.repo, cli.password_file.as_deref(), &base)?;
    match cli.operation {
        Operation::Init => restic.run(restic.command().arg("init")),
        Operation::Backup(args) => restic.backup(&args, &base),
        Operation::Restore(args) => restore::restore(&restic, &args, &base),
        Operation::Size { snapshot } => restic.size(&snapshot),
        Operation::Sort(args) => manifest::sort_files(&args.files_dir, args.dry_run),
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            if let Some(failure) = error.downcast_ref::<restic::CommandFailure>() {
                return ExitCode::from(failure.0);
            }
            eprintln!("Error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
