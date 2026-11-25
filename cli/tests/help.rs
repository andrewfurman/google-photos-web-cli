use assert_cmd::Command;

#[test]
fn help_explains_auth_search_download_and_refresh_without_authenticating() {
    let output = Command::new(assert_cmd::cargo::cargo_bin!("google-photos-web-cli"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    for phrase in [
        "auth chrome",
        "--person",
        "download PHOTO_ID",
        "GPHOTOS_SESSION",
        "inspect PHOTO_ID",
        "auth export",
        "auth keepalive",
        "EXAMPLES:",
        "FIRST-TIME SETUP",
        "SAFETY:",
        "help search",
        "--all",
        "--from",
        "upload FILE...",
        "album-create NAME",
    ] {
        assert!(text.contains(phrase), "missing {phrase}");
    }
    for command in [
        vec!["help", "search"],
        vec!["auth", "chrome", "--help"],
        vec!["download", "--help"],
        vec!["inspect", "--help"],
        vec!["people", "--help"],
        vec!["albums", "--help"],
        vec!["album-add", "--help"],
        vec!["album-create", "--help"],
        vec!["upload", "--help"],
        vec!["help", "upload"],
        vec!["tag", "--help"],
        vec!["auth", "export", "--help"],
        vec!["auth", "import", "--help"],
        vec!["auth", "keepalive", "--help"],
        vec!["help", "auth", "keepalive"],
    ] {
        Command::new(assert_cmd::cargo::cargo_bin!("google-photos-web-cli"))
            .args(command)
            .assert()
            .success();
    }
}
#[test]
fn invalid_inputs_fail_before_accessing_keychain() {
    for args in [
        vec!["search"],
        vec!["search", "beach", "--limit", "0"],
        vec!["download", "../../secret"],
        vec!["inspect", "../../secret"],
        vec!["tag", "../../secret", "--face", "0", "--person", "Alice"],
        vec!["tag", "AF1QipSyntheticPhoto", "--person", "Alice"],
        vec!["tag", "AF1QipSyntheticPhoto", "--face", "0"],
        vec![
            "tag",
            "AF1QipSyntheticPhoto",
            "--face",
            "0",
            "--face-id",
            "x",
            "--person",
            "A",
        ],
        vec![
            "tag",
            "AF1QipSyntheticPhoto",
            "--face",
            "0",
            "--person",
            "A",
            "--person-id",
            "B",
        ],
        vec!["auth", "export"],
        vec!["upload"],
        vec!["upload", "a.jpg", "--create-album"],
        vec!["upload", "a.jpg", "--album", "A", "--album-id", "B"],
        vec!["upload", "a.jpg", "--album-id", "../../secret"],
        vec!["album-create"],
        vec!["album-create", "Trip", "../../secret"],
    ] {
        Command::new(assert_cmd::cargo::cargo_bin!("google-photos-web-cli"))
            .args(args)
            .assert()
            .failure();
    }
}

#[test]
fn detailed_help_documents_filters_pagination_metadata_and_session_refresh() {
    for (args, phrases) in [
        (
            vec!["help", "search"],
            vec![
                "Advanced filters:",
                "Examples:",
                "ADVANCED SEARCH OPTIONS:",
                "google-photos-web-cli search \"birthday cake\"",
                "--page-token",
                "--sort",
                "An incomplete search reports why it stopped and how to resume",
                "--all",
                "YYYY-MM-DD",
                "--location",
                "--tag",
            ],
        ),
        (
            vec!["help", "inspect"],
            vec![
                "filename",
                "caption",
                "lens",
                "latitude/longitude",
                "timezone",
                "every detected face",
            ],
        ),
        (
            vec!["help", "tag"],
            vec![
                "--dry-run",
                "--force",
                "--person-id",
                "--face-id",
                "already tagged",
                "google-photos-web-cli inspect",
            ],
        ),
        (
            vec!["auth", "keepalive", "--help"],
            vec![
                "Examples:",
                "google-photos-web-cli auth keepalive --quiet",
                "--quiet",
                "--session-file",
                "RotateCookies",
                "every ~10 minutes",
                "auth status",
                "__Secure-1PSIDTS",
                "never cookie values",
                "docs/SESSION_REFRESH.md",
            ],
        ),
        (
            vec!["help", "upload"],
            vec![
                "Examples:",
                "google-photos-web-cli upload IMG_0001.jpg --dry-run",
                "--album",
                "--album-id",
                "--create-album",
                "--dry-run",
                "--json",
                "duplicate",
                "media key",
                "SHA-1",
                "modified time",
                "never a duplicate",
            ],
        ),
        (
            vec!["help", "album-create"],
            vec![
                "Examples:",
                "google-photos-web-cli album-create \"Trip 2026\" --dry-run",
                "--dry-run",
                "--json",
                "Never makes a duplicate",
                "case-sensitive",
            ],
        ),
        (
            vec!["help", "auth"],
            vec![
                "No single guaranteed expiry",
                "refresh token",
                "auth cookies",
                "auth export",
                "auth keepalive",
            ],
        ),
    ] {
        let out = Command::new(assert_cmd::cargo::cargo_bin!("google-photos-web-cli"))
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        for phrase in phrases {
            assert!(text.contains(phrase), "help missing {phrase}");
        }
    }
}

fn bin() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("google-photos-web-cli"))
}

#[test]
fn short_long_and_help_subcommand_give_identical_top_level_help() {
    let help = |args: &[&str]| bin().args(args).output().unwrap().stdout;
    let long = help(&["--help"]);
    assert!(!long.is_empty());
    assert_eq!(long, help(&["-h"]));
    assert_eq!(long, help(&["help"]));
    let text = String::from_utf8(long).unwrap();
    assert!(text.contains("EXAMPLES:"));
    assert_eq!(text.matches("Usage:").count(), 1);
}

#[test]
fn bare_invocation_shows_help_and_keeps_failure_exit_code() {
    let out = bin().output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8(out.stderr).unwrap().contains("EXAMPLES:"));
}

#[test]
fn mistakes_point_to_command_help() {
    let out = bin().args(["auth", "export"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(
        err.contains("google-photos-web-cli auth export --help"),
        "{err}"
    );
    let out = bin().arg("bogus").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8(out.stderr).unwrap().contains("--help"));
}

#[test]
fn album_add_requires_explicit_action_and_deduplicates_the_dry_run() {
    let base = [
        "album-add",
        "AF1QipExamplePhoto",
        "AF1QipExamplePhoto",
        "--album-id",
        "AF1QipExampleAlbum",
    ];
    bin().args(base).assert().code(2);
    bin()
        .args(base)
        .args(["--dry-run", "--yes"])
        .assert()
        .code(2);
    let output = bin()
        .args(base)
        .args(["--dry-run", "--json"])
        .assert()
        .success();
    let result: serde_json::Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
    assert_eq!(result["status"], "dry_run");
    assert_eq!(result["album_id"], "AF1QipExampleAlbum");
    assert_eq!(
        result["photo_ids"],
        serde_json::json!(["AF1QipExamplePhoto"])
    );
}

#[test]
fn upload_and_album_create_are_listed_as_writes_and_document_every_flag() {
    let text = String::from_utf8(bin().arg("--help").output().unwrap().stdout).unwrap();
    let writes = text
        .split("Change (writes to your library)")
        .nth(1)
        .unwrap();
    let writes = writes.split("EXAMPLES:").next().unwrap();
    assert!(writes.contains("upload FILE..."));
    assert!(writes.contains("album-create NAME"));
    for (command, flags) in [
        (
            "upload",
            vec![
                "--album ",
                "--album-id",
                "--create-album",
                "--dry-run",
                "--json",
            ],
        ),
        ("album-create", vec!["--dry-run", "--json"]),
    ] {
        let help =
            String::from_utf8(bin().args([command, "--help"]).output().unwrap().stdout).unwrap();
        for flag in flags {
            assert!(help.contains(flag), "{command} help missing {flag}");
        }
    }
}

#[test]
fn upload_rejects_missing_and_non_media_files_before_signing_in() {
    let dir = tempfile::tempdir().unwrap();
    let text = dir.path().join("notes.txt");
    std::fs::write(&text, b"hello").unwrap();
    let missing = dir.path().join("missing.jpg");
    for path in [&text, &missing] {
        let out = bin()
            .env("GPHOTOS_SESSION_FILE", dir.path().join("none.env"))
            .env_remove("GPHOTOS_SESSION")
            .args(["upload", "--dry-run"])
            .arg(path)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1));
    }
}
