//! These tests only read/write temporary data. They require restic on PATH.
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;

struct Project {
    temporary: TempDir,
    base: PathBuf,
    external: PathBuf,
    files: PathBuf,
}

fn checked(output: Output, code: i32) -> Output {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

impl Project {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().join("home [spaces] ü");
        let external = temporary.path().join("system [files]");
        let files = temporary.path().join("files");
        for directory in [&base, &external, &files] {
            fs::create_dir(directory).unwrap();
        }
        fs::write(files.join("example.jsonc"), "IGNORED INVALID EXAMPLE").unwrap();
        Self {
            temporary,
            base,
            external,
            files,
        }
    }

    fn root(&self) -> &Path {
        self.temporary.path()
    }

    fn configure(&self, command: &mut Command) {
        command
            .current_dir(self.root())
            .env("HOME", &self.base)
            .env("RESTIC_PASSWORD", "temporary-test-password")
            .env("RESTIC_CACHE_DIR", self.root().join("cache"))
            .env_remove("RESTIC_REPOSITORY")
            .env_remove("RESTIC_REPOSITORY_FILE")
            .env_remove("RESTIC_PASSWORD_FILE")
            .env_remove("RESTIC_PASSWORD_COMMAND")
            .env_remove("RESTIC_HOST");
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_backup_parser"));
        self.configure(&mut command);
        command
    }

    fn cli(&self, arguments: &[&str], code: i32) -> Output {
        checked(self.command().args(arguments).output().unwrap(), code)
    }

    fn restic(&self, arguments: &[&OsStr]) -> Output {
        let mut command = Command::new("restic");
        self.configure(&mut command);
        command
            .arg("--repo")
            .arg(self.root().join("repo"))
            .current_dir(&self.base)
            .args(arguments);
        checked(
            command
                .output()
                .expect("Install restic to run integration tests"),
            0,
        )
    }

    fn snapshots(&self) -> Vec<Snapshot> {
        serde_json::from_slice(
            &self
                .restic(&[OsStr::new("snapshots"), OsStr::new("--json")])
                .stdout,
        )
        .unwrap()
    }

    fn manifest(&self, name: &str, include: &[&str], exclude: &[&str]) {
        #[derive(Serialize)]
        struct Manifest<'a> {
            include: &'a [&'a str],
            exclude: &'a [&'a str],
        }
        fs::write(
            self.files.join(name),
            serde_json::to_vec(&Manifest { include, exclude }).unwrap(),
        )
        .unwrap();
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.base.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn restore(&self, snapshot: &str, destination: &Path, options: &[&str]) -> Output {
        fs::create_dir_all(destination).unwrap();
        checked(
            self.command()
                .args(["restore", snapshot, "--base-path"])
                .arg(destination)
                .args(options)
                .output()
                .unwrap(),
            0,
        )
    }
}

#[derive(Deserialize)]
struct Snapshot {
    id: String,
    tags: Option<Vec<String>>,
    parent: Option<String>,
}

#[test]
fn backup_exclusions_restore_and_dry_runs() {
    let project = Project::new();
    project.cli(&["init"], 0);
    for name in [
        "A/B/secret",
        "A/C/keep",
        "A/D/keep",
        "A/BB/keep",
        "A/[literal]/secret",
        "A/literal/keep",
        "A/cached-one/secret",
        "A/.hidden/keep",
        "notes [literal].txt",
        "#note\nsecond line",
        "wildcard one.save",
        "wildcard two.save",
        "other/B/keep",
    ] {
        project.write(name, name);
    }
    fs::write(project.external.join("config.txt"), "system configuration").unwrap();
    project.manifest(
        "a.jsonc",
        &[
            "A/",
            "A/B/",
            "notes [literal].txt",
            "#note\nsecond line",
            "wildcard *.save",
            "other/",
            project.external.to_str().unwrap(),
        ],
        &[],
    );
    // Exclusions in another manifest win over every include, including explicit roots.
    project.manifest(
        "b.json",
        &["A/B/secret"],
        &["A/B/", "A/[literal]/", "A/cached-*/"],
    );
    project.cli(&["backup"], 0);
    let snapshots = project.snapshots();
    assert_eq!(snapshots.len(), 1);
    assert!(
        snapshots[0]
            .tags
            .as_ref()
            .unwrap()
            .iter()
            .any(|tag| tag == "backup-layout:absolute-v1")
    );
    assert!(
        snapshots[0]
            .tags
            .as_ref()
            .unwrap()
            .iter()
            .any(|tag| tag.contains("home%20%5Bspaces%5D%20%C3%BC"))
    );
    let id = &snapshots[0].id;
    let restored = project.root().join("restored");
    fs::write(project.external.join("config.txt"), "changed").unwrap();
    project.restore(id, &restored, &["--verify"]);
    for name in [
        "A/C/keep",
        "A/D/keep",
        "A/BB/keep",
        "A/literal/keep",
        "A/.hidden/keep",
        "notes [literal].txt",
        "#note\nsecond line",
        "wildcard one.save",
        "wildcard two.save",
        "other/B/keep",
    ] {
        assert_eq!(fs::read_to_string(restored.join(name)).unwrap(), name);
    }
    for name in ["A/B", "A/[literal]", "A/cached-one"] {
        assert!(!restored.join(name).exists(), "restored excluded {name}");
    }
    assert_eq!(
        fs::read_to_string(project.external.join("config.txt")).unwrap(),
        "system configuration"
    );

    project.cli(&["backup", "--dry-run"], 0);
    assert_eq!(project.snapshots().len(), 1);
    let dry_target = project.root().join("dry target");
    project.restore(id, &dry_target, &["--dry-run"]);
    assert_eq!(fs::read_dir(dry_target).unwrap().count(), 0);

    let subfolder = project.root().join("subfolder");
    checked(
        project
            .command()
            .args([
                "restore",
                &format!("{id}:{}/A/C", project.base.display()),
                "--target",
            ])
            .arg(&subfolder)
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(
        fs::read_to_string(subfolder.join("keep")).unwrap(),
        "A/C/keep"
    );
}

#[test]
fn legacy_relative_and_absolute_snapshots_and_staging() {
    let project = Project::new();
    project.cli(&["init"], 0);
    project.write("Desktop [project]/hello", "hello");
    project.write(".hidden/config", "hidden");
    fs::write(project.external.join("config"), "system").unwrap();
    // Match the original scripts' mixed relative-home/absolute-system arguments.
    project.restic(&[
        OsStr::new("backup"),
        OsStr::new("--"),
        OsStr::new("Desktop [project]"),
        OsStr::new(".hidden"),
        project.external.as_os_str(),
    ]);
    let id = project.snapshots()[0].id.clone();
    let destination = project.root().join("legacy relative");
    project.restore(
        &id,
        &destination,
        &["--source-base", project.base.to_str().unwrap(), "--verify"],
    );
    assert_eq!(
        fs::read_to_string(destination.join("Desktop [project]/hello")).unwrap(),
        "hello"
    );
    assert_eq!(
        fs::read_to_string(destination.join(".hidden/config")).unwrap(),
        "hidden"
    );

    project.restic(&[
        OsStr::new("backup"),
        OsStr::new("--"),
        project.base.join(".hidden").as_os_str(),
        project.external.as_os_str(),
    ]);
    let id = project.snapshots().last().unwrap().id.clone();
    let destination = project.root().join("legacy absolute");
    project.restore(
        &id,
        &destination,
        &["--source-base", project.base.to_str().unwrap()],
    );
    assert_eq!(
        fs::read_to_string(destination.join(".hidden/config")).unwrap(),
        "hidden"
    );

    let staging = project.root().join("staging");
    let output = project
        .command()
        .args(["restore", &id, "--target"])
        .arg(&staging)
        .output()
        .unwrap();
    // Restricted uid mappings may prevent restoring ownership of /tmp ancestors.
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("lchown") && stderr.contains("invalid argument"),
            "{stderr}"
        );
        assert!(
            stderr
                .lines()
                .all(|line| line.contains("lchown") || line.starts_with("Fatal:")),
            "{stderr}"
        );
    }
    assert_eq!(
        fs::read_to_string(
            staging
                .join(project.base.strip_prefix("/").unwrap())
                .join(".hidden/config")
        )
        .unwrap(),
        "hidden"
    );
    assert_eq!(
        fs::read_to_string(
            staging
                .join(project.external.strip_prefix("/").unwrap())
                .join("config")
        )
        .unwrap(),
        "system"
    );
}

#[test]
fn validation_sort_atomicity_and_permissions() {
    let project = Project::new();
    let manifest = project.files.join("a.jsonc");
    let original = "\u{feff}{\n  \"include\": [\n    // header\n    \"z\", // z note\n    // a note\n    \"a\",\n  ],\n  \"exclude\": [\"z\", \"a\"],\n}\n";
    fs::write(&manifest, original).unwrap();
    fs::set_permissions(&manifest, fs::Permissions::from_mode(0o640)).unwrap();
    fs::write(
        project.files.join("z.jsonc"),
        r#"{"include": [1], "exclude": []}"#,
    )
    .unwrap();
    project.cli(&["sort"], 1);
    assert_eq!(fs::read_to_string(&manifest).unwrap(), original);
    project.cli(&["backup"], 1); // Fails before restic tries to open a repository.
    assert!(!project.root().join("repo").exists());
    fs::remove_file(project.files.join("z.jsonc")).unwrap();
    project.cli(&["sort", "--dry-run"], 0);
    assert_eq!(fs::read_to_string(&manifest).unwrap(), original);
    project.cli(&["sort"], 0);
    let sorted = fs::read_to_string(&manifest).unwrap();
    assert!(sorted.contains("// a note\n    \"a\""));
    assert!(sorted.contains("\"z\", // z note"));
    assert!(sorted.contains("\"exclude\": [\"a\", \"z\"]"));
    assert_eq!(
        fs::metadata(&manifest).unwrap().permissions().mode() & 0o777,
        0o640
    );
    project.manifest("a.jsonc", &["A/", "A/B/"], &["A/"]);
    let output = project.cli(&["backup"], 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("no backup paths after exclusions"));
}

#[test]
fn missing_input_preserves_restic_exit_three_and_password_path() {
    let project = Project::new();
    fs::write(project.base.join("password"), "temporary-test-password\n").unwrap();
    checked(
        project
            .command()
            .env_remove("RESTIC_PASSWORD")
            .args(["init", "--password-file", "password"])
            .output()
            .unwrap(),
        0,
    );
    project.write("exists", "content");
    project.manifest("paths.json", &["exists", "missing"], &[]);
    project.cli(&["backup"], 3);
    assert_eq!(project.snapshots().len(), 1);
}

#[test]
fn prune_reclaims_forgotten_data_and_preserves_retained_snapshot() {
    let project = Project::new();
    project.cli(&["prune"], 10); // Preserve restic's missing-repository exit code.
    project.cli(&["init"], 0);
    project.write("content", "old content");
    project.manifest("paths.json", &["content"], &[]);
    project.cli(&["backup"], 0);
    let forgotten = project.snapshots()[0].id.clone();
    project.write("content", "retained content");
    project.cli(&["backup"], 0);
    project.restic(&[OsStr::new("forget"), OsStr::new(&forgotten)]);
    let retained = project.snapshots()[0].id.clone();

    let data = project.root().join("repo/data");
    let packs: Vec<_> = fs::read_dir(&data)
        .unwrap()
        .flat_map(|directory| fs::read_dir(directory.unwrap().path()).unwrap())
        .map(|entry| {
            let path = entry.unwrap().path();
            let contents = fs::read(&path).unwrap();
            (path, contents)
        })
        .collect();
    assert!(!packs.is_empty());
    project.cli(&["prune", "--dry-run"], 0);
    for (path, contents) in &packs {
        assert_eq!(&fs::read(path).unwrap(), contents);
    }

    fs::write(project.base.join("password"), "temporary-test-password\n").unwrap();
    checked(
        project
            .command()
            .env_remove("RESTIC_PASSWORD")
            .args(["prune", "--repo", "repo", "--password-file", "password"])
            .output()
            .unwrap(),
        0,
    );
    assert!(packs.iter().any(|(path, _)| !path.exists()));
    let snapshots = project.snapshots();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].id, retained);
    project.restic(&[OsStr::new("check"), OsStr::new("--read-data")]);
    let destination = project.root().join("pruned restore");
    project.restore(&retained, &destination, &["--verify"]);
    assert_eq!(
        fs::read_to_string(destination.join("content")).unwrap(),
        "retained content"
    );
}

#[test]
fn list_reads_password_from_inherited_stdin() {
    let project = Project::new();
    project.cli(&["init"], 0);
    project.write("content", "snapshot content");
    project.manifest("paths.json", &["content"], &[]);
    project.cli(&["backup"], 0);
    let snapshot = project.snapshots()[0].id.clone();
    let password = project.root().join("stdin-password");
    fs::write(&password, "temporary-test-password\n").unwrap();
    for arguments in [vec!["list"], vec!["list", "--json"], vec!["snapshots"]] {
        let output = checked(
            project
                .command()
                .args(arguments)
                .env_remove("RESTIC_PASSWORD")
                .stdin(fs::File::open(&password).unwrap())
                .output()
                .unwrap(),
            0,
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains(&snapshot));
    }
}

#[test]
fn list_select_parents_restore_and_delete_snapshots() {
    let project = Project::new();
    project.cli(&["list"], 10);
    project.cli(&["init"], 0);
    let empty = project.cli(&["list", "--json"], 0);
    assert!(
        serde_json::from_slice::<Vec<serde_json::Value>>(&empty.stdout)
            .unwrap()
            .is_empty()
    );
    assert!(String::from_utf8_lossy(&project.cli(&["list"], 0).stdout).contains("No snapshots"));
    project.cli(&["restore", "1"], 1);
    project.cli(&["delete"], 2); // Deletion requires an explicit selection.

    project.write("content", "first snapshot");
    project.manifest("paths.json", &["content"], &[]);
    project.cli(&["backup"], 0);
    let first = project.snapshots()[0].id.clone();
    project.write("content", "second snapshot");
    project.cli(&["backup", "1"], 0);
    let snapshots = project.snapshots();
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[1].parent.as_deref(), Some(first.as_str()));
    project.write("content", "third snapshot");
    let prefix = format!("id:{}", &first[..12]);
    project.cli(&["backup", "--parent", &prefix], 0);
    let snapshots = project.snapshots();
    assert_eq!(snapshots.len(), 3);
    assert_eq!(snapshots[2].parent.as_deref(), Some(first.as_str()));
    let latest = snapshots[2].id.clone();
    project.cli(&["backup", "latest", "--dry-run"], 0);
    project.cli(&["backup", "--snapshot", "1", "--dry-run"], 0);
    project.cli(&["backup", "1", "--parent", "latest"], 2);
    project.cli(&["backup", "99"], 1);
    assert_eq!(project.snapshots().len(), 3);

    let listing = checked(
        project
            .command()
            .args(["snapshots", "--json"])
            .env("RESTIC_HOST", "a-host-that-does-not-exist")
            .output()
            .unwrap(),
        0,
    );
    let listing: Vec<serde_json::Value> = serde_json::from_slice(&listing.stdout).unwrap();
    assert_eq!(listing.len(), 3);
    let text = String::from_utf8(project.cli(&["list"], 0).stdout).unwrap();
    assert!(text.starts_with("NUMBER\tID\tDATE\tHOST\tPATHS\n"));
    for (index, snapshot) in listing.iter().enumerate() {
        assert_eq!(snapshot["number"], index + 1);
        assert_eq!(snapshot["id"], snapshots[index].id);
        assert!(text.contains(snapshot["id"].as_str().unwrap()));
        assert!(text.contains(snapshot["time"].as_str().unwrap()));
        assert!(text.contains(snapshot["hostname"].as_str().unwrap()));
        assert!(text.contains(snapshot["paths"][0].as_str().unwrap()));
    }
    let restored = project.root().join("numbered restore");
    project.restore("1", &restored, &["--verify"]);
    assert_eq!(
        fs::read_to_string(restored.join("content")).unwrap(),
        "first snapshot"
    );
    project.restore(&prefix, &restored, &[]);
    project.restore("latest", &restored, &[]);
    assert_eq!(
        fs::read_to_string(restored.join("content")).unwrap(),
        "third snapshot"
    );
    let subtree = project.root().join("numbered subtree");
    checked(
        project
            .command()
            .args([
                "restore",
                &format!("1:{}", project.base.display()),
                "--target",
            ])
            .arg(&subtree)
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(
        fs::read_to_string(subtree.join("content")).unwrap(),
        "first snapshot"
    );

    project.cli(&["delete", "1", "2", "--dry-run", "--prune"], 0);
    assert_eq!(project.snapshots().len(), 3);
    project.cli(&["delete", "1", "99"], 1); // Validate every selector before deleting.
    project.cli(&["delete", "1", "--all"], 2);
    assert_eq!(project.snapshots().len(), 3);
    project.cli(&["delete", "1", &first, "2"], 0); // Resolve numbers once and deduplicate IDs.
    let snapshots = project.snapshots();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].id, latest);
    let listing: Vec<serde_json::Value> =
        serde_json::from_slice(&project.cli(&["list", "--json"], 0).stdout).unwrap();
    assert_eq!(listing[0]["number"], 1);
    assert_eq!(listing[0]["id"], latest);
    project.cli(&["forget", "latest", "--prune"], 0);
    assert!(project.snapshots().is_empty());
}

#[test]
fn size_exports_native_ncdu_and_removes_temporary_file() {
    let project = Project::new();
    project.cli(&["init"], 0);
    project.write("exists", "content");
    project.manifest("paths.json", &["exists"], &[]);
    project.cli(&["backup"], 0);
    let first = project.snapshots()[0].id.clone();
    project.write("new-file", "new snapshot content");
    project.manifest("paths.json", &["new-file"], &[]);
    project.cli(&["backup"], 0);
    let bin = project.root().join("bin");
    fs::create_dir(&bin).unwrap();
    let shell = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|path| path.join("sh"))
        .find(|path| path.is_file())
        .expect("sh is needed for the test helper");
    let helper = bin.join("gdu");
    fs::write(&helper, format!("#!{}\n[ \"$1\" = -f ] || exit 10\ncp -- \"$2\" \"$GDU_CAPTURE\"\nprintf '%s' \"$2\" > \"$GDU_PATH_CAPTURE\"\n", shell.display())).unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
    let capture = project.root().join("export.json");
    let path_capture = project.root().join("export-path");
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let search_path = std::env::join_paths(paths).unwrap();
    let prefix = format!("id:{}", &first[..12]);
    for (selectors, expected_file) in [
        (vec![], "new-file"),
        (vec!["1"], "exists"),
        (vec![prefix.as_str()], "exists"),
        (vec!["latest"], "new-file"),
    ] {
        checked(
            project
                .command()
                .arg("size")
                .args(selectors)
                .env("PATH", &search_path)
                .env("GDU_CAPTURE", &capture)
                .env("GDU_PATH_CAPTURE", &path_capture)
                .output()
                .unwrap(),
            0,
        );
        let export = fs::read_to_string(&capture).unwrap();
        assert!(export.contains("restic"));
        assert!(export.contains(expected_file));
        let export_path = fs::read_to_string(&path_capture).unwrap();
        assert!(!Path::new(&export_path).exists());
    }
}
