use anyhow::{Context, Result, ensure};
use glob::{MatchOptions, Pattern};
use path_clean::PathClean;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

/// Expand home syntax against the selected base, without expanding other variables.
pub fn from_base(value: &str, base: &Path) -> PathBuf {
    if matches!(value, "~" | "$HOME" | "${HOME}") {
        return base.to_path_buf();
    }
    for prefix in ["~/", "$HOME/", "${HOME}/"] {
        if let Some(relative) = value.strip_prefix(prefix) {
            return base.join(relative);
        }
    }
    base.join(value)
}

pub fn caller_path(path: &Path) -> Result<PathBuf> {
    let expanded = if path == Path::new("~") || path.starts_with("~/") {
        let home = std::env::var_os("HOME").context("HOME is unset; cannot expand ~")?;
        PathBuf::from(home).join(path.strip_prefix("~")?)
    } else {
        path.to_path_buf()
    };
    Ok(std::path::absolute(expanded)?.clean())
}

pub fn base_directory(path: &Path) -> Result<PathBuf> {
    let path = fs::canonicalize(caller_path(path)?)
        .with_context(|| format!("Cannot resolve base directory {}", path.display()))?;
    ensure!(
        path.is_dir(),
        "Base path is not a directory: {}",
        path.display()
    );
    Ok(path)
}

/// Existing literal names (including dangling symlinks) take priority over globbing.
pub fn expand(value: &str, base: &Path) -> Result<Vec<PathBuf>> {
    let path = from_base(value, base);
    if fs::symlink_metadata(&path).is_ok() {
        return Ok(vec![path.clean()]);
    }
    let text = path.to_str().context("Glob paths must be UTF-8")?;
    // The base directory's own brackets or stars must never become globs.
    let pattern = if let Ok(relative) = path.strip_prefix(base) {
        let prefix = Pattern::escape(
            base.to_str()
                .context("Base must be UTF-8 for glob expansion")?,
        );
        format!("{prefix}/{}", relative.display())
    } else {
        text.to_owned()
    };
    let options = MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: true,
    };
    let mut matches = glob::glob_with(&pattern, options)
        .with_context(|| format!("Invalid glob {value:?}"))?
        .map(|entry| entry.map(|path| path.clean()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    matches.sort();
    // Preserve missing sources so restic reports an incomplete backup.
    if matches.is_empty() {
        matches.push(path.clean());
    }
    Ok(matches)
}

pub fn unique(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

/// Escape a literal absolute path for restic's Go-style filter patterns.
pub fn literal_pattern(path: &Path) -> Result<String> {
    let mut pattern = String::new();
    for ch in path
        .to_str()
        .context("Restic filter paths must be UTF-8")?
        .chars()
    {
        if matches!(ch, '\\' | '*' | '?' | '[' | ']') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    Ok(pattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_home_spelling_uses_the_selected_base() {
        let base = Path::new("/chosen/home [literal]");
        for value in ["~", "$HOME", "${HOME}"] {
            assert_eq!(from_base(value, base), base);
        }
        for value in ["notes", "~/notes", "$HOME/notes", "${HOME}/notes"] {
            assert_eq!(from_base(value, base), base.join("notes"));
        }
        assert_eq!(from_base("/etc/nixos", base), Path::new("/etc/nixos"));
        assert_eq!(from_base("$OTHER/notes", base), base.join("$OTHER/notes"));
    }

    #[test]
    fn globs_escape_base_and_preserve_literal_names_and_missing_sources() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let base = temporary.path().join("base [literal]");
        fs::create_dir(&base)?;
        for name in ["one.save", "two.save", ".hidden.save", "notes [draft]"] {
            fs::write(base.join(name), "content")?;
        }
        assert_eq!(
            expand("*.save", &base)?,
            vec![base.join("one.save"), base.join("two.save")]
        );
        assert_eq!(
            expand("notes [draft]", &base)?,
            vec![base.join("notes [draft]")]
        );
        assert_eq!(expand("missing*", &base)?, vec![base.join("missing*")]);
        Ok(())
    }
}
