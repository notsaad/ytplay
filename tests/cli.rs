use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::TempDir;

#[test]
fn plays_stream_with_fake_tools() {
    let temp = TempDir::new().unwrap();
    let bin_dir = temp.path().join("bin");
    fs::create_dir(&bin_dir).unwrap();

    let mpv_log = temp.path().join("mpv.log");
    let ytdlp_log = temp.path().join("yt-dlp.log");

    write_executable(
        &bin_dir.join("yt-dlp"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\nprintf '%s\\n' 'https://stream.example/audio'\n",
            ytdlp_log.display()
        ),
    );
    write_executable(
        &bin_dir.join("mpv"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\nexit 0\n",
            mpv_log.display()
        ),
    );

    let output = Command::new(binary_path())
        .arg("https://www.youtube.com/watch?v=test123")
        .env("PATH", &bin_dir)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");

    let ytdlp_args = fs::read_to_string(ytdlp_log).unwrap();
    assert!(ytdlp_args.contains("--no-playlist"));
    assert!(ytdlp_args.contains("--get-url"));
    assert!(ytdlp_args.contains("https://www.youtube.com/watch?v=test123"));

    let mpv_args = fs::read_to_string(mpv_log).unwrap();
    assert!(mpv_args.contains("--no-video"));
    assert!(mpv_args.contains("--cache-on-disk=no"));
    assert!(mpv_args.contains("https://stream.example/audio"));
}

#[test]
fn surfaces_ytdlp_failures() {
    let temp = TempDir::new().unwrap();
    let bin_dir = temp.path().join("bin");
    fs::create_dir(&bin_dir).unwrap();

    write_executable(
        &bin_dir.join("yt-dlp"),
        "#!/bin/sh\nprintf '%s\\n' 'extract failed' >&2\nexit 7\n",
    );
    write_executable(&bin_dir.join("mpv"), "#!/bin/sh\nexit 0\n");

    let output = Command::new(binary_path())
        .arg("https://www.youtube.com/watch?v=broken")
        .env("PATH", &bin_dir)
        .output()
        .unwrap();

    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("yt-dlp"));
    assert!(stderr.contains("extract failed"));
}

#[test]
fn reports_missing_dependency() {
    let temp = TempDir::new().unwrap();

    let output = Command::new(binary_path())
        .arg("https://www.youtube.com/watch?v=test123")
        .env("PATH", temp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Required dependency"));
    assert!(stderr.contains("brew install yt-dlp mpv"));
}

#[test]
fn accepts_url_from_piped_stdin() {
    let temp = TempDir::new().unwrap();
    let bin_dir = temp.path().join("bin");
    fs::create_dir(&bin_dir).unwrap();

    let mpv_log = temp.path().join("mpv.log");

    write_executable(
        &bin_dir.join("yt-dlp"),
        "#!/bin/sh\nprintf '%s\\n' 'https://stream.example/from-stdin'\n",
    );
    write_executable(
        &bin_dir.join("mpv"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\nexit 0\n",
            mpv_log.display()
        ),
    );

    let mut child = Command::new(binary_path())
        .env("PATH", &bin_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"https://www.youtube.com/watch?v=piped123\n")
        .unwrap();

    let output = child.wait_with_output().unwrap();

    assert!(output.status.success(), "{output:?}");

    let mpv_args = fs::read_to_string(mpv_log).unwrap();
    assert!(mpv_args.contains("https://stream.example/from-stdin"));
}

fn binary_path() -> &'static str {
    env!("CARGO_BIN_EXE_ytplay")
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}
