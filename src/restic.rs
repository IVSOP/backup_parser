use crate::{cli::ManifestArgs, manifest, paths};
use anyhow::{Context, Result};
use percent_encoding::{AsciiSet, CONTROLS, percent_encode};
use std::{
    ffi::{OsStr, OsString},
    fmt,
    io::Write,
    os::unix::{ffi::OsStrExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
};
use tempfile::NamedTempFile;

pub const LAYOUT_TAG: &str = "backup-layout:absolute-v1";
pub const BASE_TAG: &str = "backup-base:";
// urllib.parse.quote(..., safe="/") compatibility for the Python snapshot tags.
const TAG_ESCAPE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

#[derive(Debug)]
pub struct CommandFailure(pub u8);
impl fmt::Display for CommandFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Command exited with status {}", self.0)
    }
}
impl std::error::Error for CommandFailure {}

fn exit_code(status: ExitStatus) -> u8 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)) as u8
}

pub struct Restic {
    repository: OsString,
    password_file: Option<PathBuf>,
    base: PathBuf,
}

impl Restic {
    pub fn new(repository: &OsStr, password_file: Option<&Path>, base: &Path) -> Result<Self> {
        let path = Path::new(repository);
        let remote = repository.as_bytes().contains(&b':')
            && !path.is_absolute()
            && !path.starts_with(".")
            && !path.starts_with("~");
        let repository = if remote {
            repository.to_owned()
        } else {
            paths::caller_path(path)?.into_os_string()
        };
        let password_file = password_file
            .map(|path| {
                let text = path.to_str().context("Password path must be UTF-8")?;
                Ok::<_, anyhow::Error>(paths::from_base(text, base))
            })
            .transpose()?;
        Ok(Self {
            repository,
            password_file,
            base: base.to_path_buf(),
        })
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new("restic");
        command
            .arg("--repo")
            .arg(&self.repository)
            .current_dir(&self.base);
        if let Some(password_file) = &self.password_file {
            command.arg("--password-file").arg(password_file);
        }
        command
    }

    pub fn run(&self, command: &mut Command) -> Result<u8> {
        Ok(exit_code(
            command
                .status()
                .with_context(|| format!("Cannot run {command:?}"))?,
        ))
    }

    pub fn output(&self, command: &mut Command) -> Result<Vec<u8>> {
        let result = command
            .stderr(Stdio::inherit())
            .output()
            .with_context(|| format!("Cannot run {command:?}"))?;
        if !result.status.success() {
            return Err(CommandFailure(exit_code(result.status)).into());
        }
        Ok(result.stdout)
    }

    pub fn backup(&self, args: &ManifestArgs, base: &Path) -> Result<u8> {
        let sources = manifest::sources(&args.files_dir, base)?;
        let mut input = NamedTempFile::new()?;
        for path in sources.include {
            input.write_all(path.as_os_str().as_bytes())?;
            input.write_all(&[0])?;
        }
        input.flush()?;
        let mut command = self.command();
        command
            .args([
                "backup",
                "--compression",
                "max",
                "--verbose",
                "--files-from-raw",
            ])
            .arg(input.path())
            .args(["--tag", LAYOUT_TAG, "--tag"])
            .arg(format!(
                "{BASE_TAG}{}",
                percent_encode(base.as_os_str().as_bytes(), TAG_ESCAPE)
            ));
        for path in sources.exclude {
            command.arg("--exclude").arg(paths::literal_pattern(&path)?);
        }
        if args.dry_run {
            command.arg("--dry-run");
        }
        self.run(&mut command)
    }

    pub fn size(&self, snapshot: &str) -> Result<u8> {
        let export = NamedTempFile::new()?;
        let code = self.run(
            self.command()
                .args(["ls", "--ncdu", "--", snapshot])
                .stdout(export.reopen()?),
        )?;
        if code != 0 {
            return Ok(code);
        }
        self.run(
            Command::new("gdu")
                .arg("-f")
                .arg(export.path())
                .current_dir(&self.base),
        )
    }
}
