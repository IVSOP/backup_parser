use crate::{cli::DeleteArgs, restic::Restic};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::{self, Write},
    path::PathBuf,
};

#[derive(Debug, Deserialize, Serialize)]
pub struct Snapshot {
    pub id: String,
    time: String,
    hostname: String,
    paths: Vec<PathBuf>,
    #[serde(flatten)]
    metadata: serde_json::Map<String, serde_json::Value>,
}

pub struct Catalog {
    snapshots: Vec<Snapshot>,
}

impl Catalog {
    pub fn load(restic: &Restic) -> Result<Self> {
        // Restic sorts this ungrouped listing chronologically, oldest first.
        // Clear RESTIC_HOST's implicit filter so numbers cover the entire repository.
        let output = restic.output(restic.command().args([
            "snapshots",
            "--json",
            "--group-by",
            "",
            "--host",
            "",
        ]))?;
        Ok(Self {
            snapshots: serde_json::from_slice(&output).context("Invalid restic snapshots JSON")?,
        })
    }

    pub fn resolve(&self, selector: &str) -> Result<&Snapshot> {
        ensure!(!self.snapshots.is_empty(), "No snapshots in repository");
        if selector == "latest" {
            return Ok(self.snapshots.last().expect("catalog is not empty"));
        }
        // Decimal selectors are list numbers. id: forces ID interpretation,
        // including for an all-numeric hexadecimal ID or prefix.
        let forced_id = selector.strip_prefix("id:");
        if forced_id.is_none()
            && !selector.is_empty()
            && selector.bytes().all(|byte| byte.is_ascii_digit())
        {
            let number: usize = selector.parse().context("Snapshot number is too large")?;
            return number
                .checked_sub(1)
                .and_then(|index| self.snapshots.get(index))
                .with_context(|| {
                    format!(
                        "Snapshot number {number} is out of range (1..={})",
                        self.snapshots.len()
                    )
                });
        }
        let prefix = forced_id.unwrap_or(selector);
        ensure!(
            !prefix.is_empty() && prefix.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Invalid snapshot selector {selector:?}; use a number from list, ID/prefix, or latest"
        );
        let prefix = prefix.to_ascii_lowercase();
        let mut matches = self
            .snapshots
            .iter()
            .filter(|snapshot| snapshot.id.starts_with(&prefix));
        let snapshot = matches
            .next()
            .with_context(|| format!("No snapshot matches {selector:?}"))?;
        ensure!(
            matches.next().is_none(),
            "Ambiguous snapshot selector {selector:?}; use a longer ID prefix"
        );
        Ok(snapshot)
    }
}

/// Resolve an optional subtree suffix while retaining restic's ID:/path syntax.
pub fn resolve_tree(restic: &Restic, selector: &str) -> Result<String> {
    let (prefix, selector) = match selector.strip_prefix("id:") {
        Some(selector) => ("id:", selector),
        None => ("", selector),
    };
    let (snapshot, subtree) = selector
        .split_once(':')
        .map_or((selector, None), |(id, path)| (id, Some(path)));
    let catalog = Catalog::load(restic)?;
    let snapshot = catalog.resolve(&format!("{prefix}{snapshot}"))?;
    match subtree {
        Some(path) => {
            ensure!(path.starts_with('/'), "Snapshot subtree must start with /");
            Ok(format!("{}:{path}", snapshot.id))
        }
        None => Ok(snapshot.id.clone()),
    }
}

pub fn list(restic: &Restic, json: bool) -> Result<u8> {
    let catalog = Catalog::load(restic)?;
    let mut output = io::stdout().lock();
    if json {
        #[derive(Serialize)]
        struct NumberedSnapshot<'a> {
            number: usize,
            #[serde(flatten)]
            snapshot: &'a Snapshot,
        }
        let snapshots: Vec<_> = catalog
            .snapshots
            .iter()
            .enumerate()
            .map(|(index, snapshot)| NumberedSnapshot {
                number: index + 1,
                snapshot,
            })
            .collect();
        serde_json::to_writer(&mut output, &snapshots)?;
        writeln!(output)?;
    } else if catalog.snapshots.is_empty() {
        writeln!(output, "No snapshots in repository")?;
    } else {
        writeln!(output, "NUMBER\tID\tDATE\tHOST\tPATHS")?;
        for (index, snapshot) in catalog.snapshots.iter().enumerate() {
            let paths: Vec<_> = snapshot
                .paths
                .iter()
                .map(|path| format!("{}", path.display()))
                .collect();
            writeln!(
                output,
                "{}\t{}\t{}\t{}\t{}",
                index + 1,
                snapshot.id,
                snapshot.time,
                snapshot.hostname,
                paths.join(", ")
            )?;
        }
    }
    Ok(0)
}

pub fn delete(restic: &Restic, args: &DeleteArgs) -> Result<u8> {
    let catalog = Catalog::load(restic)?;
    // Resolve every selector before deleting anything, so deleting one number
    // cannot shift the meaning of another number in the same command.
    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    for selector in &args.snapshots {
        let id = &catalog.resolve(selector)?.id;
        if seen.insert(id) {
            ids.push(id);
        }
    }
    let mut command = restic.command();
    command.arg("forget");
    if args.dry_run {
        command.arg("--dry-run");
    }
    if args.prune {
        command.arg("--prune");
    }
    restic.run(command.arg("--").args(ids))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectors_require_a_unique_snapshot_and_disambiguate_numeric_ids() {
        let catalog = Catalog {
            snapshots: serde_json::from_value(serde_json::json!([
                {"id": "123abc", "time": "older", "hostname": "host", "paths": []},
                {"id": "abcdef", "time": "newer", "hostname": "host", "paths": []},
                {"id": "abc123", "time": "newest", "hostname": "host", "paths": []}
            ]))
            .unwrap(),
        };
        assert_eq!(catalog.resolve("1").unwrap().id, "123abc");
        assert_eq!(catalog.resolve("latest").unwrap().id, "abc123");
        assert_eq!(catalog.resolve("ABCD").unwrap().id, "abcdef");
        assert_eq!(catalog.resolve("id:123").unwrap().id, "123abc");
        for selector in ["", "0", "4", "123", "abc", "ffff", "--all", "id:", "1:/"] {
            assert!(catalog.resolve(selector).is_err(), "accepted {selector:?}");
        }
        assert!(Catalog { snapshots: vec![] }.resolve("latest").is_err());
    }
}
