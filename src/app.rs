use std::env;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;

use crate::player;

const INSTALL_HINT: &str = "Install dependencies with: brew install yt-dlp mpv";

#[derive(Debug, Parser)]
#[command(
    name = "ytplay",
    about = "Play audio from a YouTube URL in your terminal"
)]
struct Cli {
    /// YouTube URL to play
    url: Option<String>,
}

#[derive(Debug, Clone)]
struct Dependencies {
    yt_dlp: PathBuf,
    mpv: PathBuf,
}

pub fn run() -> Result<u8> {
    let cli = Cli::parse();
    let stdin_is_terminal = io::stdin().is_terminal();
    let url = resolve_url(
        cli.url,
        stdin_is_terminal,
        &mut io::stdin().lock(),
        &mut io::stdout(),
    )?;
    let deps = Dependencies::detect()?;
    let use_terminal_ui = stdin_is_terminal && io::stdout().is_terminal();

    let stream = extract_stream(&deps.yt_dlp, &url)?;
    player::play_stream(
        &deps.mpv,
        &stream.stream_url,
        stream.title.as_deref(),
        use_terminal_ui,
    )
}

fn resolve_url(
    cli_url: Option<String>,
    interactive: bool,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<String> {
    if let Some(url) = cli_url {
        return normalize_url(url);
    }

    if !interactive {
        let mut buffer = String::new();
        input
            .read_line(&mut buffer)
            .context("failed to read URL from stdin")?;

        if !buffer.trim().is_empty() {
            return normalize_url(buffer);
        }

        bail!(
            "No URL provided. Pass a quoted YouTube URL as an argument, pipe one on stdin, or run from an interactive terminal."
        );
    }

    write!(output, "Paste YouTube URL: ").context("failed to write prompt")?;
    output.flush().context("failed to flush prompt")?;

    let mut buffer = String::new();
    input
        .read_line(&mut buffer)
        .context("failed to read URL from stdin")?;

    normalize_url(buffer)
}

fn normalize_url(url: String) -> Result<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        bail!("URL cannot be empty.");
    }

    Ok(trimmed.to_owned())
}

impl Dependencies {
    fn detect() -> Result<Self> {
        Ok(Self {
            yt_dlp: find_on_path("yt-dlp").ok_or_else(|| missing_dependency_error("yt-dlp"))?,
            mpv: find_on_path("mpv").ok_or_else(|| missing_dependency_error("mpv"))?,
        })
    }
}

fn missing_dependency_error(tool: &str) -> anyhow::Error {
    anyhow!("Required dependency `{tool}` was not found on PATH. {INSTALL_HINT}")
}

fn find_on_path(tool: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;

    env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(tool);
        if is_executable(&candidate) {
            Some(candidate)
        } else {
            None
        }
    })
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
    }

    #[cfg(not(unix))]
    {
        true
    }
}

struct ExtractedStream {
    title: Option<String>,
    stream_url: String,
}

fn extract_stream(yt_dlp_path: &Path, youtube_url: &str) -> Result<ExtractedStream> {
    let output = yt_dlp_command(yt_dlp_path, youtube_url)
        .output()
        .with_context(|| format!("failed to start `{}`", yt_dlp_path.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "`yt-dlp` failed with status {}: {}",
            format_exit_status(&output.status),
            stderr.trim()
        );
    }

    let stdout = String::from_utf8(output.stdout).context("yt-dlp returned non-UTF-8 output")?;
    parse_stream_output(&stdout)
}

fn yt_dlp_command(program: &Path, youtube_url: &str) -> Command {
    let mut command = Command::new(program);
    command.args([
        "--no-playlist",
        "--no-warnings",
        "--get-title",
        "-f",
        "bestaudio/best",
        "--get-url",
        youtube_url,
    ]);
    command
}

pub(crate) fn mpv_command(program: &Path, stream_url: &str) -> Command {
    let mut command = Command::new(program);
    command.args([
        "--no-video",
        "--cache=yes",
        "--cache-secs=2",
        "--cache-on-disk=no",
        "--demuxer-max-bytes=8MiB",
        "--demuxer-max-back-bytes=1MiB",
    ]);
    command.arg(stream_url);
    command
}

fn format_exit_status(status: &std::process::ExitStatus) -> String {
    status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "terminated by signal".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn uses_cli_argument_without_prompting() {
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let url = resolve_url(
            Some(" https://youtube.com/watch?v=abc ".to_string()),
            true,
            &mut input,
            &mut output,
        )
        .unwrap();

        assert_eq!(url, "https://youtube.com/watch?v=abc");
        assert!(output.is_empty());
    }

    #[test]
    fn prompts_for_url_in_interactive_mode() {
        let mut input = Cursor::new(b"https://youtu.be/demo\n".to_vec());
        let mut output = Vec::new();

        let url = resolve_url(None, true, &mut input, &mut output).unwrap();

        assert_eq!(url, "https://youtu.be/demo");
        assert_eq!(String::from_utf8(output).unwrap(), "Paste YouTube URL: ");
    }

    #[test]
    fn rejects_missing_url_in_non_interactive_mode() {
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let error = resolve_url(None, false, &mut input, &mut output).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("Pass a quoted YouTube URL as an argument")
        );
    }

    #[test]
    fn reads_url_from_piped_stdin_in_non_interactive_mode() {
        let mut input = Cursor::new(b"https://www.youtube.com/watch?v=stdin123\n".to_vec());
        let mut output = Vec::new();

        let url = resolve_url(None, false, &mut input, &mut output).unwrap();

        assert_eq!(url, "https://www.youtube.com/watch?v=stdin123");
        assert!(output.is_empty());
    }

    #[test]
    fn builds_yt_dlp_command_with_expected_flags() {
        let command = yt_dlp_command(Path::new("/bin/yt-dlp"), "https://example.com/watch?v=1");

        let args: Vec<String> = command_lossy_args(&command);
        assert_eq!(
            args,
            vec![
                "--no-playlist",
                "--no-warnings",
                "--get-title",
                "-f",
                "bestaudio/best",
                "--get-url",
                "https://example.com/watch?v=1",
            ]
        );
    }

    #[test]
    fn parses_title_and_stream_url_from_yt_dlp_output() {
        let extracted =
            parse_stream_output("Example Title\nhttps://stream.example/audio\n").unwrap();

        assert_eq!(extracted.title.as_deref(), Some("Example Title"));
        assert_eq!(extracted.stream_url, "https://stream.example/audio");
    }

    #[test]
    fn parses_stream_url_without_title_for_legacy_output() {
        let extracted = parse_stream_output("https://stream.example/audio\n").unwrap();

        assert_eq!(extracted.title, None);
        assert_eq!(extracted.stream_url, "https://stream.example/audio");
    }

    #[test]
    fn builds_mpv_command_with_low_memory_flags() {
        let command = mpv_command(Path::new("/bin/mpv"), "https://stream.example/audio");

        let args: Vec<String> = command_lossy_args(&command);
        assert_eq!(
            args,
            vec![
                "--no-video",
                "--cache=yes",
                "--cache-secs=2",
                "--cache-on-disk=no",
                "--demuxer-max-bytes=8MiB",
                "--demuxer-max-back-bytes=1MiB",
                "https://stream.example/audio",
            ]
        );
    }

    #[test]
    fn missing_dependency_error_mentions_brew_install() {
        let message = missing_dependency_error("yt-dlp").to_string();
        assert!(message.contains("brew install yt-dlp mpv"));
    }

    fn command_lossy_args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }
}

fn parse_stream_output(output: &str) -> Result<ExtractedStream> {
    let lines: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();

    match lines.as_slice() {
        [stream_url] => Ok(ExtractedStream {
            title: None,
            stream_url: (*stream_url).to_string(),
        }),
        [title, stream_url, ..] => Ok(ExtractedStream {
            title: Some((*title).to_string()),
            stream_url: (*stream_url).to_string(),
        }),
        [] => Err(anyhow!("`yt-dlp` did not return a playable stream URL.")),
    }
}
