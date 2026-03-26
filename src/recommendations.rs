use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

const SEARCH_RESULTS: usize = 5;

#[derive(Debug, Clone)]
pub(crate) struct RecommendationSeed {
    pub(crate) title: String,
    pub(crate) uploader: Option<String>,
    pub(crate) current_video_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct UpNextCandidate {
    pub(crate) video_id: String,
    pub(crate) title: String,
    pub(crate) uploader: Option<String>,
}

pub(crate) type RecommendationsReceiver = Receiver<Result<Vec<UpNextCandidate>>>;

impl UpNextCandidate {
    pub(crate) fn playback_input(&self) -> String {
        self.video_id.clone()
    }

    pub(crate) fn display_label(&self) -> String {
        match self.uploader.as_deref() {
            Some(uploader) if !uploader.trim().is_empty() => {
                format!("{} | {}", self.title, uploader)
            }
            _ => self.title.clone(),
        }
    }
}

pub(crate) fn spawn_recommendation_fetch(
    yt_dlp_path: PathBuf,
    seed: RecommendationSeed,
) -> RecommendationsReceiver {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let result = fetch_recommendations(&yt_dlp_path, &seed);
        let _ = tx.send(result);
    });

    rx
}

fn fetch_recommendations(
    yt_dlp_path: &Path,
    seed: &RecommendationSeed,
) -> Result<Vec<UpNextCandidate>> {
    let primary_query = build_search_query(seed, true);
    let primary = search_candidates(
        yt_dlp_path,
        &primary_query,
        seed.current_video_id.as_deref(),
    )?;

    if !primary.is_empty() || seed.uploader.is_none() {
        return Ok(primary);
    }

    let fallback_query = build_search_query(seed, false);
    search_candidates(
        yt_dlp_path,
        &fallback_query,
        seed.current_video_id.as_deref(),
    )
}

fn build_search_query(seed: &RecommendationSeed, include_uploader: bool) -> String {
    match (include_uploader, seed.uploader.as_deref()) {
        (true, Some(uploader)) if !uploader.trim().is_empty() => {
            format!("{} {}", uploader.trim(), seed.title.trim())
        }
        _ => seed.title.trim().to_string(),
    }
}

fn search_candidates(
    yt_dlp_path: &Path,
    query: &str,
    current_video_id: Option<&str>,
) -> Result<Vec<UpNextCandidate>> {
    let output = recommendation_command(yt_dlp_path, query)
        .output()
        .with_context(|| format!("failed to start `{}`", yt_dlp_path.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "`yt-dlp` recommendation search failed: {}",
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8(output.stdout)
        .context("yt-dlp returned non-UTF-8 recommendation output")?;
    parse_recommendations(&stdout, current_video_id)
}

fn recommendation_command(program: &Path, query: &str) -> Command {
    let mut command = Command::new(program);
    command.args([
        "--no-warnings",
        "--flat-playlist",
        "--dump-single-json",
        &format!("ytsearch{}:{query}", SEARCH_RESULTS),
    ]);
    command
}

fn parse_recommendations(
    output: &str,
    current_video_id: Option<&str>,
) -> Result<Vec<UpNextCandidate>> {
    let payload: Value =
        serde_json::from_str(output).context("failed to parse yt-dlp recommendation JSON")?;
    let entries = payload
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("yt-dlp recommendation response was missing entries"))?;

    let mut candidates = Vec::new();

    for entry in entries {
        let Some(video_id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        if current_video_id == Some(video_id) {
            continue;
        }
        if candidates
            .iter()
            .any(|candidate: &UpNextCandidate| candidate.video_id == video_id)
        {
            continue;
        }

        let Some(title) = entry.get("title").and_then(Value::as_str) else {
            continue;
        };

        let uploader = entry
            .get("uploader")
            .or_else(|| entry.get("channel"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        candidates.push(UpNextCandidate {
            video_id: video_id.to_string(),
            title: title.to_string(),
            uploader,
        });
    }

    Ok(candidates)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_search_query_with_uploader_when_available() {
        let seed = RecommendationSeed {
            title: "Never Gonna Give You Up".to_string(),
            uploader: Some("Rick Astley".to_string()),
            current_video_id: Some("dQw4w9WgXcQ".to_string()),
        };

        assert_eq!(
            build_search_query(&seed, true),
            "Rick Astley Never Gonna Give You Up"
        );
        assert_eq!(build_search_query(&seed, false), "Never Gonna Give You Up");
    }

    #[test]
    fn parses_recommendations_and_filters_current_video_and_duplicates() {
        let parsed = parse_recommendations(
            r#"{
                "entries": [
                    {"id": "current123", "title": "Current", "uploader": "Uploader"},
                    {"id": "next1", "title": "Next One", "uploader": "Artist A"},
                    {"id": "next1", "title": "Next One Duplicate", "uploader": "Artist A"},
                    {"id": "next2", "title": "Next Two", "channel": "Artist B"}
                ]
            }"#,
            Some("current123"),
        )
        .unwrap();

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].video_id, "next1");
        assert_eq!(parsed[0].display_label(), "Next One | Artist A");
        assert_eq!(parsed[1].video_id, "next2");
        assert_eq!(parsed[1].uploader.as_deref(), Some("Artist B"));
    }
}
