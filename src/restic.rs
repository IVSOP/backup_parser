use crate::{cli::BackupArgs, manifest, paths, snapshots};
use anyhow::{Context, Result};
use percent_encoding::{AsciiSet, CONTROLS, percent_encode};
use std::{
    env,
    ffi::{OsStr, OsString},
    fmt,
    io::{self, BufRead, IsTerminal, Write},
    os::unix::{ffi::OsStrExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
};
use tempfile::NamedTempFile;

pub const LAYOUT_TAG: &str = "backup-layout:absolute-v1";
pub const BASE_TAG: &str = "backup-base:";
const WRONG_PASSWORD: u8 = 12;
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
    password: Option<Vec<u8>>,
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
            password: None,
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
            self.start(command)?
                .wait()
                .with_context(|| format!("Cannot wait for {command:?}"))?,
        ))
    }

    pub fn output(&self, command: &mut Command) -> Result<Vec<u8>> {
        let result = self
            .start(command.stdout(Stdio::piped()))?
            .wait_with_output()
            .with_context(|| format!("Cannot wait for {command:?}"))?;
        if !result.status.success() {
            return Err(CommandFailure(exit_code(result.status)).into());
        }
        Ok(result.stdout)
    }

    pub fn output_with_reusable_password(&mut self, command: &mut Command) -> Result<Vec<u8>> {
        if self.password_file.is_some()
            || [
                "RESTIC_PASSWORD",
                "RESTIC_PASSWORD_FILE",
                "RESTIC_PASSWORD_COMMAND",
            ]
            .iter()
            .any(|name| env::var_os(name).is_some_and(|value| !value.is_empty()))
        {
            return self.output(command);
        }

        let stdin = io::stdin();
        if !stdin.is_terminal() {
            let mut password = Vec::new();
            stdin
                .lock()
                .read_until(b'\n', &mut password)
                .context("Cannot read repository password from stdin")?;
            self.password = Some(password);
            return self.output(command);
        }

        // Match Restic's three interactive attempts while opening the repository.
        let mut attempts_left = 3;
        loop {
            let mut password = rpassword::prompt_password("enter password for repository: ")
                .context("Cannot read repository password")?
                .into_bytes();
            password.push(b'\n');
            self.password = Some(password);
            let result = self.output(command);
            attempts_left -= 1;
            let wrong_password = result.as_ref().is_err_and(|error| {
                matches!(
                    error.downcast_ref::<CommandFailure>(),
                    Some(CommandFailure(WRONG_PASSWORD))
                )
            });
            if attempts_left == 0 || !wrong_password {
                return result;
            }
            eprintln!("Try again.");
        }
    }

    fn start(&self, command: &mut Command) -> Result<Child> {
        let Some(password) = &self.password else {
            return command
                .spawn()
                .with_context(|| format!("Cannot run {command:?}"));
        };
        let mut child = command
            .stdin(Stdio::piped())
            .spawn()
            .with_context(|| format!("Cannot run {command:?}"))?;
        // Close the pipe after writing so Restic sees EOF. Never add the password
        // to the command arguments or environment.
        let result = child
            .stdin
            .take()
            .expect("piped child stdin")
            .write_all(password);
        if let Err(error) = result
            && error.kind() != io::ErrorKind::BrokenPipe
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("Cannot pass repository password to Restic");
        }
        Ok(child)
    }

    pub fn backup(&mut self, args: &BackupArgs, base: &Path) -> Result<u8> {
        let sources = manifest::sources(&args.manifest.files_dir, base)?;
        let parent = args
            .parent
            .as_deref()
            .or(args.snapshot.as_deref())
            .map(|selector| {
                snapshots::Catalog::load_for_operation(self)?
                    .resolve(selector)
                    .map(|s| s.id.clone())
            })
            .transpose()?;
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
        if let Some(parent) = parent {
            command.arg("--parent").arg(parent);
        }
        if args.manifest.dry_run {
            command.arg("--dry-run");
        }
        self.run(&mut command)
    }

    pub fn size(&mut self, snapshot: &str) -> Result<u8> {
        let catalog = snapshots::Catalog::load_for_operation(self)?;
        let (_, snapshot) = catalog.resolve_tree(snapshot)?;
        let export = NamedTempFile::new()?;
        let code = self.run(
            self.command()
                .args(["ls", "--ncdu", "--", &snapshot])
                .stdout(export.reopen()?),
        )?;
        if code != 0 {
            return Ok(code);
        }
        Ok(exit_code(
            Command::new("gdu")
                .arg("-f")
                .arg(export.path())
                .current_dir(&self.base)
                .status()
                .context("Cannot run gdu")?,
        ))
    }
}
