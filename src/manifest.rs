use crate::paths;
use anyhow::{Context, Result, ensure};
use jsonc_parser::{ParseOptions, cst::CstRootNode};
use nix::unistd::{Gid, Uid, fchown};
use serde::Deserialize;
use std::{
    fs,
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
use tempfile::NamedTempFile;

#[derive(Debug, Deserialize)]
#[serde(try_from = "String")]
pub struct PathEntry(String);

impl TryFrom<String> for PathEntry {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value.contains('\0') {
            Err("Paths must be nonempty strings without NUL characters")
        } else {
            Ok(Self(value))
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub include: Vec<PathEntry>,
    pub exclude: Vec<PathEntry>,
}

fn options() -> ParseOptions {
    ParseOptions {
        allow_comments: true,
        allow_trailing_commas: true,
        allow_loose_object_property_names: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
        allow_bare_decimal_point_numbers: false,
        allow_non_finite_numbers: false,
        allow_extended_string_escapes: false,
    }
}

fn read(path: &Path) -> Result<String> {
    Ok(fs::read_to_string(path)
        .with_context(|| format!("Cannot read {}", path.display()))?
        .trim_start_matches('\u{feff}')
        .to_owned())
}

fn parse(text: &str) -> Result<Manifest> {
    Ok(jsonc_parser::parse_to_serde_value(text, &options())?)
}

pub fn files(directory: &Path) -> Result<Vec<PathBuf>> {
    let directory = paths::caller_path(directory)?;
    let mut files = Vec::new();
    for entry in fs::read_dir(&directory)
        .with_context(|| format!("Cannot read manifest directory {}", directory.display()))?
    {
        let path = entry?.path();
        if matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("json" | "jsonc")
        ) && path.file_stem().is_none_or(|name| name != "example")
            && path.is_file()
        {
            files.push(path);
        }
    }
    files.sort();
    ensure!(
        !files.is_empty(),
        "No JSON/JSONC manifests found in {}",
        directory.display()
    );
    Ok(files)
}

pub struct Sources {
    pub include: Vec<PathBuf>,
    pub exclude: Vec<PathBuf>,
}

pub fn sources(directory: &Path, base: &Path) -> Result<Sources> {
    // Parse every manifest before expansion or invoking restic.
    let manifests = files(directory)?
        .into_iter()
        .map(|path| {
            parse(&read(&path)?).with_context(|| format!("Invalid manifest {}", path.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut include = Vec::new();
    let mut exclude = Vec::new();
    for manifest in manifests {
        for entry in manifest.include {
            include.extend(paths::expand(&entry.0, base)?);
        }
        for entry in manifest.exclude {
            exclude.extend(paths::expand(&entry.0, base)?);
        }
    }
    let exclude = paths::unique(exclude);
    let include = paths::unique(include)
        .into_iter()
        .filter(|path| !exclude.iter().any(|excluded| path.starts_with(excluded)))
        .collect::<Vec<_>>();
    ensure!(
        !include.is_empty(),
        "Manifests contain no backup paths after exclusions"
    );
    Ok(Sources { include, exclude })
}

fn sorted_text(text: &str) -> Result<String> {
    parse(text)?;
    let root = CstRootNode::parse(text, &options())?;
    let object = root.object_value().context("Expected a manifest object")?;
    for name in ["include", "exclude"] {
        let array = object.array_value(name).context("Expected a path array")?;
        // Opening comments remain headers; other notes travel with entries.
        array
            .sort_elements()
            .pin_comment_headers_with(move |node, comments| {
                if node.element_index() == Some(0) {
                    comments.len()
                } else {
                    0
                }
            })
            .by_key(|node| {
                node.as_string_lit()
                    .expect("validated string array")
                    .decoded_value()
                    .expect("validated JSON string")
            });
    }
    Ok(root.to_string())
}

pub fn sort_files(directory: &Path, dry_run: bool) -> Result<u8> {
    let plans = files(directory)?
        .into_iter()
        .map(|path| {
            let text = sorted_text(&read(&path)?)
                .with_context(|| format!("Invalid manifest {}", path.display()))?;
            Ok((path, text))
        })
        .collect::<Result<Vec<_>>>()?;
    // Validate all input before replacing anything. Each replacement is atomic.
    for (path, text) in plans {
        println!("Sorting {}", path.display());
        if dry_run {
            continue;
        }
        let original = fs::metadata(&path)?;
        let mut output = NamedTempFile::new_in(path.parent().context("Manifest has no parent")?)?;
        output.write_all(text.as_bytes())?;
        if Uid::effective().is_root() {
            fchown(
                output.as_file(),
                Some(Uid::from_raw(original.uid())),
                Some(Gid::from_raw(original.gid())),
            )?;
        }
        output.as_file().set_permissions(original.permissions())?;
        output.as_file().sync_all()?;
        output
            .persist(&path)
            .with_context(|| format!("Cannot replace {}", path.display()))?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_schema_and_paths() {
        for text in [
            r#"["old array"]"#,
            r#"{"include": [], "exclude": [], "typo": []}"#,
            r#"{"include": [1], "exclude": []}"#,
            r#"{"include": [""], "exclude": []}"#,
            r#"{"include": ["\u0000"], "exclude": []}"#,
            r#"{"include": []}"#,
            r#"{"include": [], "include": [], "exclude": []}"#,
        ] {
            assert!(parse(text).is_err(), "accepted {text}");
        }
    }

    #[test]
    fn sorts_decoded_strings_and_preserves_notes() -> Result<()> {
        let text = "// object header\n{\n  \"include\": [\n    // array header\n    \"z\", // z note\n    // a note\n    \"a\",\n    \"\\u0062\",\n    // footer\n  ],\n  \"exclude\": [\"z\", \"a\"],\n}\n";
        let sorted = sorted_text(text)?;
        let manifest = parse(&sorted)?;
        assert_eq!(
            manifest
                .include
                .iter()
                .map(|p| p.0.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "z"]
        );
        assert!(sorted.contains("\"z\", // z note"));
        assert!(sorted.contains("// a note\n    \"a\""));
        assert!(sorted.find("// array header").unwrap() < sorted.find("\"a\"").unwrap());
        assert!(sorted.contains("// footer"));
        assert_eq!(sorted_text(&sorted)?, sorted);
        Ok(())
    }
}
