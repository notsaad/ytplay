use std::env;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use serde_json::Value;

use crate::player::{self, PlaybackOutcome};
use crate::recommendations::{self, RecommendationSeed};

const INSTALL_HINT: &str = "Install dependencies with: brew install yt-dlp mpv";

#[derive(Debug, Parser)]
#[command(about = "Play audio from a YouTube URL in your terminal")]
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
    let mut url = resolve_url(
        cli.url,
        stdin_is_terminal,
        &mut io::stdin().lock(),
        &mut io::stdout(),
    )?;
    let deps = Dependencies::detect()?;
    let use_terminal_ui = stdin_is_terminal && io::stdout().is_terminal();

    loop {
        let stream = extract_stream(&deps.yt_dlp, &url)?;
        let recommendations = if use_terminal_ui {
            stream
                .recommendation_seed()
                .map(|seed| recommendations::spawn_recommendation_fetch(deps.yt_dlp.clone(), seed))
        } else {
            None
        };

        match player::play_stream(
            &deps.mpv,
            &stream.stream_url,
            Some(&stream.title),
            use_terminal_ui,
            recommendations,
        )? {
            PlaybackOutcome::Finished(code) => return Ok(code),
            PlaybackOutcome::PlayNext(next_input) => {
                url = next_input;
            }
        }
    }
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

    if looks_like_youtube_id(trimmed) {
        return Ok(format!("https://www.youtube.com/watch?v={trimmed}"));
    }

    if looks_like_youtube_url_without_scheme(trimmed) {
        return Ok(format!("https://{trimmed}"));
    }

    Ok(trimmed.to_owned())
}

fn looks_like_youtube_id(input: &str) -> bool {
    input.len() == 11
        && input
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn looks_like_youtube_url_without_scheme(input: &str) -> bool {
    input.starts_with("youtu.be/")
        || input.starts_with("youtube.com/")
        || input.starts_with("www.youtube.com/")
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
        fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }

    #[cfg(not(unix))]
    {
        true
    }
}

struct ExtractedStream {
    title: String,
    stream_url: String,
    video_id: Option<String>,
    uploader: Option<String>,
}

fn extract_stream(yt_dlp_path: &Path, youtube_url: &str) -> Result<ExtractedStream> {
    let metadata = extract_video_metadata(yt_dlp_path, youtube_url)?;
    let output = yt_dlp_stream_command(yt_dlp_path, youtube_url)
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
    Ok(ExtractedStream {
        title: metadata.title,
        stream_url: parse_stream_output(&stdout)?,
        video_id: metadata.video_id,
        uploader: metadata.uploader,
    })
}

impl ExtractedStream {
    fn recommendation_seed(&self) -> Option<RecommendationSeed> {
        if self.title.trim().is_empty() {
            return None;
        }

        Some(RecommendationSeed {
            title: self.title.clone(),
            uploader: self.uploader.clone(),
            current_video_id: self.video_id.clone(),
        })
    }
}

struct VideoMetadata {
    title: String,
    video_id: Option<String>,
    uploader: Option<String>,
}

fn extract_video_metadata(yt_dlp_path: &Path, youtube_url: &str) -> Result<VideoMetadata> {
    let output = yt_dlp_metadata_command(yt_dlp_path, youtube_url)
        .output()
        .with_context(|| format!("failed to start `{}`", yt_dlp_path.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "`yt-dlp` metadata lookup failed with status {}: {}",
            format_exit_status(&output.status),
            stderr.trim()
        );
    }

    let stdout = String::from_utf8(output.stdout).context("yt-dlp returned non-UTF-8 metadata")?;
    parse_video_metadata(&stdout)
}

fn yt_dlp_metadata_command(program: &Path, youtube_url: &str) -> Command {
    let mut command = Command::new(program);
    command.args([
        "--no-playlist",
        "--no-warnings",
        "--skip-download",
        "--dump-single-json",
        youtube_url,
    ]);
    command
}

fn yt_dlp_stream_command(program: &Path, youtube_url: &str) -> Command {
    let mut command = Command::new(program);
    command.args([
        "--no-playlist",
        "--no-warnings",
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

fn parse_video_metadata(output: &str) -> Result<VideoMetadata> {
    let payload: Value =
        serde_json::from_str(output).context("failed to parse yt-dlp metadata JSON")?;

    let title = payload
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("yt-dlp metadata was missing a title"))?
        .to_string();
    let video_id = payload
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let uploader = payload
        .get("uploader")
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    Ok(VideoMetadata {
        title: recommendations::sanitize_title(&title),
        video_id,
        uploader,
    })
}

fn parse_stream_output(output: &str) -> Result<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("`yt-dlp` did not return a playable stream URL."))
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
        let command =
            yt_dlp_stream_command(Path::new("/bin/yt-dlp"), "https://example.com/watch?v=1");

        let args: Vec<String> = command_lossy_args(&command);
        assert_eq!(
            args,
            vec![
                "--no-playlist",
                "--no-warnings",
                "-f",
                "bestaudio/best",
                "--get-url",
                "https://example.com/watch?v=1",
            ]
        );
    }

    #[test]
    fn parses_video_metadata_json() {
        let metadata = parse_video_metadata(
            r#"{"id":"abc123","title":"Example Title","uploader":"Example Uploader"}"#,
        )
        .unwrap();

        assert_eq!(metadata.video_id.as_deref(), Some("abc123"));
        assert_eq!(metadata.title, "Example Title");
        assert_eq!(metadata.uploader.as_deref(), Some("Example Uploader"));
    }

    #[test]
    fn parses_video_metadata_and_strips_branding_suffix() {
        let metadata = parse_video_metadata(
            r#"{"id":"abc123","title":"Example Title | PoweredbyREC.","uploader":"Example Uploader"}"#,
        )
        .unwrap();

        assert_eq!(metadata.title, "Example Title");
    }

    #[test]
    fn parses_stream_url_output() {
        let stream_url = parse_stream_output("https://stream.example/audio\n").unwrap();
        assert_eq!(stream_url, "https://stream.example/audio");
    }

    #[test]
    fn normalizes_bare_video_id_to_watch_url() {
        let normalized = normalize_url("dQw4w9WgXcQ".to_string()).unwrap();
        assert_eq!(normalized, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
    }

    #[test]
    fn adds_scheme_to_youtube_host_without_one() {
        let normalized = normalize_url("youtu.be/dQw4w9WgXcQ".to_string()).unwrap();
        assert_eq!(normalized, "https://youtu.be/dQw4w9WgXcQ");
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
