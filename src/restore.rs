use crate::{
    cli::{Layout, RestoreArgs},
    paths,
    restic::{BASE_TAG, LAYOUT_TAG, Restic},
};
use anyhow::{Context, Result, bail, ensure};
use path_clean::PathClean;
use percent_encoding::percent_decode_str;
use serde::Deserialize;
use std::{
    collections::HashSet,
    ffi::OsString,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
};

#[derive(Debug, Deserialize)]
struct Snapshot {
    id: String,
    paths: Vec<PathBuf>,
    #[serde(default)]
    tags: Option<Vec<String>>,
}

impl Snapshot {
    fn tags(&self) -> &[String] {
        self.tags.as_deref().unwrap_or_default()
    }
}

#[derive(Deserialize)]
#[serde(tag = "struct_type")]
enum Listing {
    #[serde(rename = "node")]
    Node { path: PathBuf },
    #[serde(other)]
    Other,
}

fn saved_base(snapshot: &Snapshot, override_base: Option<&Path>, base: &Path) -> Result<PathBuf> {
    if let Some(path) = override_base {
        ensure!(
            path.is_absolute(),
            "--source-base must be an absolute path inside the snapshot"
        );
        return Ok(path.clean());
    }
    for tag in snapshot.tags() {
        if let Some(value) = tag.strip_prefix(BASE_TAG) {
            let path = PathBuf::from(OsString::from_vec(percent_decode_str(value).collect()));
            ensure!(
                path.is_absolute(),
                "Snapshot base tag must contain an absolute path"
            );
            return Ok(path.clean());
        }
    }
    // Infer source homes from paths, never the username (which may be root under sudo).
    let mut homes = HashSet::new();
    for path in &snapshot.paths {
        for prefix in ["/home", "/Users"] {
            if let Ok(relative) = path.strip_prefix(prefix)
                && let Some(name) = relative.components().next()
            {
                homes.insert(Path::new(prefix).join(name));
            }
        }
        if path.starts_with("/root") {
            homes.insert(PathBuf::from("/root"));
        }
    }
    ensure!(
        homes.len() <= 1,
        "Snapshot contains multiple home directories; pass --source-base or --target"
    );
    Ok(homes
        .into_iter()
        .next()
        .unwrap_or_else(|| base.to_path_buf()))
}

#[derive(Debug, PartialEq, Eq)]
struct RestoreJob {
    selector: String,
    target: PathBuf,
    includes: Vec<String>,
}

fn candidates(snapshot: &Snapshot, source_base: &Path) -> Result<Vec<(PathBuf, Option<PathBuf>)>> {
    snapshot
        .paths
        .iter()
        .map(|path| {
            ensure!(
                path.is_absolute(),
                "Snapshot source path must be absolute: {}",
                path.display()
            );
            let relative = path
                .strip_prefix(source_base)
                .ok()
                .map(|relative| Path::new("/").join(relative));
            Ok((path.clone(), relative))
        })
        .collect()
}

fn plan(
    snapshot: &Snapshot,
    source_base: &Path,
    base: &Path,
    layout: Layout,
    candidates: Vec<(PathBuf, Option<PathBuf>)>,
    present: &HashSet<PathBuf>,
) -> Result<Vec<RestoreJob>> {
    let absolute = snapshot.tags().iter().any(|tag| tag == LAYOUT_TAG);
    let mut jobs: Vec<RestoreJob> = Vec::new();
    for (path, relative) in candidates {
        let choices = match &relative {
            None => vec![&path],
            Some(_) if absolute || layout == Layout::Absolute => vec![&path],
            Some(relative) if layout == Layout::Relative => vec![relative],
            Some(relative) => {
                if relative == &path {
                    vec![&path]
                } else {
                    vec![&path, relative]
                }
            }
        };
        let choices = choices
            .into_iter()
            .filter(|path| present.contains(*path))
            .collect::<Vec<_>>();
        ensure!(
            choices.len() <= 1,
            "Ambiguous legacy path {}; pass --layout absolute or --layout relative, or use --target",
            path.display()
        );
        let chosen = choices.first().with_context(|| {
            format!(
                "Cannot locate snapshot source {}; check --source-base/--layout, or use --target",
                path.display()
            )
        })?;
        let (root, target, include) = if relative.is_none() {
            let parent = chosen.parent().unwrap_or(Path::new("/"));
            let include = Path::new("/").join(chosen.strip_prefix(parent)?);
            (parent.to_path_buf(), parent.to_path_buf(), include)
        } else if **chosen == path {
            (
                source_base.to_path_buf(),
                base.to_path_buf(),
                Path::new("/").join(path.strip_prefix(source_base)?),
            )
        } else {
            (PathBuf::from("/"), base.to_path_buf(), (*chosen).clone())
        };
        let selector = if root == Path::new("/") {
            snapshot.id.clone()
        } else {
            format!(
                "{}:{}",
                snapshot.id,
                root.to_str().context("Snapshot subtree must be UTF-8")?
            )
        };
        let include = paths::literal_pattern(&include)?;
        if let Some(job) = jobs
            .iter_mut()
            .find(|job| job.selector == selector && job.target == target)
        {
            job.includes.push(include);
        } else {
            jobs.push(RestoreJob {
                selector,
                target,
                includes: vec![include],
            });
        }
    }
    Ok(jobs)
}

fn restore_jobs(restic: &Restic, args: &RestoreArgs, base: &Path) -> Result<Vec<RestoreJob>> {
    let output = restic.output(restic.command().args([
        "--no-lock",
        "snapshots",
        "--json",
        "--",
        &args.snapshot,
    ]))?;
    let mut snapshots: Vec<Snapshot> =
        serde_json::from_slice(&output).context("Invalid restic snapshots JSON")?;
    ensure!(
        snapshots.len() == 1,
        "Expected one snapshot for {:?}, found {}; use its ID",
        args.snapshot,
        snapshots.len()
    );
    let snapshot = snapshots.pop().context("Snapshot is missing")?;
    let source_base = saved_base(&snapshot, args.source_base.as_deref(), base)?;
    let candidates = candidates(&snapshot, &source_base)?;
    // Query only parents of candidate sources, avoiding a recursive scan of the snapshot.
    let mut parents = candidates
        .iter()
        .flat_map(|(path, relative)| {
            std::iter::once(path.as_path())
                .chain(relative.as_deref())
                .filter_map(Path::parent)
        })
        .collect::<Vec<_>>();
    parents.sort();
    parents.dedup();
    let output = restic.output(
        restic
            .command()
            .args(["--no-lock", "ls", "--json", "--", &snapshot.id])
            .args(parents),
    )?;
    let mut present = HashSet::from([PathBuf::from("/")]);
    for line in output
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
    {
        if let Listing::Node { path } =
            serde_json::from_slice(line).context("Invalid restic ls JSON")?
        {
            present.insert(path);
        }
    }
    plan(
        &snapshot,
        &source_base,
        base,
        args.layout,
        candidates,
        &present,
    )
}

pub fn restore(restic: &Restic, args: &RestoreArgs, base: &Path) -> Result<u8> {
    let target = args.target.as_deref().or(args.positional_target.as_deref());
    let jobs = if target.is_some() || args.snapshot.contains(':') {
        vec![RestoreJob {
            selector: args.snapshot.clone(),
            target: target
                .map(paths::caller_path)
                .transpose()?
                .unwrap_or_else(|| base.to_path_buf()),
            includes: Vec::new(),
        }]
    } else {
        restore_jobs(restic, args, base)?
    };
    if jobs.is_empty() {
        bail!("Snapshot contains no paths");
    }
    // Construct and validate the full plan before restoring any files.
    let commands = jobs
        .into_iter()
        .map(|job| {
            let mut command = restic.command();
            command.arg("restore").arg("--target").arg(job.target);
            for include in job.includes {
                command.arg("--include").arg(include);
            }
            if args.dry_run {
                command.arg("--dry-run");
            }
            if args.verify {
                command.arg("--verify");
            }
            command.arg("--").arg(job.selector);
            command
        })
        .collect::<Vec<_>>();
    for mut command in commands {
        println!("{command:?}");
        let code = restic.run(&mut command)?;
        if code != 0 {
            return Ok(code);
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_mixed_plan_and_ambiguity() -> Result<()> {
        let snapshot = Snapshot {
            id: "abc".into(),
            tags: None,
            paths: vec![
                "/home/old/Desktop".into(),
                "/home/old/.ssh".into(),
                "/services/app".into(),
            ],
        };
        let base = Path::new("/home/new");
        let source = saved_base(&snapshot, None, base)?;
        assert_eq!(source, Path::new("/home/old"));
        let mut present = ["/", "/Desktop", "/.ssh", "/services/app"]
            .map(PathBuf::from)
            .into_iter()
            .collect::<HashSet<_>>();
        let jobs = plan(
            &snapshot,
            &source,
            base,
            Layout::Auto,
            candidates(&snapshot, &source)?,
            &present,
        )?;
        assert_eq!(
            jobs,
            vec![
                RestoreJob {
                    selector: "abc".into(),
                    target: base.into(),
                    includes: vec!["/Desktop".into(), "/.ssh".into()]
                },
                RestoreJob {
                    selector: "abc:/services".into(),
                    target: "/services".into(),
                    includes: vec!["/app".into()]
                }
            ]
        );
        present.insert("/home/old/Desktop".into());
        assert!(
            plan(
                &snapshot,
                &source,
                base,
                Layout::Auto,
                candidates(&snapshot, &source)?,
                &present
            )
            .is_err()
        );
        Ok(())
    }
}
