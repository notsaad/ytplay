use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tempfile::{Builder, TempDir};

use crate::app::mpv_command;
use crate::recommendations::{RecommendationsReceiver, UpNextCandidate};
use crate::ui::{PlaybackUi, PlaybackView, UpNextOverlayView};

const SOCKET_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(50);
const UI_VOLUME_STEP: f64 = 5.0;
const MPV_VOLUME_MULTIPLIER: f64 = 2.0;
const SEEK_SECONDS: i64 = 30;
const UP_NEXT_AUTOPLAY_DELAY: Duration = Duration::from_secs(8);

#[derive(Debug, Clone)]
pub(crate) struct PlaybackState {
    pub(crate) title: String,
    pub(crate) time_pos: f64,
    pub(crate) duration: Option<f64>,
    pub(crate) paused: bool,
    pub(crate) volume: f64,
    pub(crate) muted: bool,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            title: "Loading stream...".to_string(),
            time_pos: 0.0,
            duration: None,
            paused: false,
            volume: 100.0,
            muted: false,
        }
    }
}

#[derive(Debug)]
pub(crate) enum Control {
    TogglePause,
    SeekBackward,
    SeekForward,
    VolumeDown,
    VolumeUp,
    ToggleMute,
    ToggleUpNext,
    SelectUpNext(usize),
    ConfirmUpNext,
    CloseOverlay,
    Quit,
}

pub(crate) enum PlaybackOutcome {
    Finished(u8),
    PlayNext(String),
}

#[derive(Debug)]
enum PlayerEvent {
    PropertyChange {
        name: String,
        data: Value,
    },
    EndFile {
        reason: Option<String>,
        error: Option<String>,
    },
    Shutdown,
    Disconnected,
}

pub(crate) fn play_stream(
    mpv_path: &Path,
    stream_url: &str,
    title: Option<&str>,
    use_terminal_ui: bool,
    recommendations: Option<RecommendationsReceiver>,
) -> Result<PlaybackOutcome> {
    if use_terminal_ui {
        return play_stream_with_ui(mpv_path, stream_url, title, recommendations);
    }

    play_stream_simple(mpv_path, stream_url)
}

fn play_stream_simple(mpv_path: &Path, stream_url: &str) -> Result<PlaybackOutcome> {
    let status = mpv_command(mpv_path, stream_url)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to start `{}`", mpv_path.display()))?;

    Ok(PlaybackOutcome::Finished(exit_code(&status)))
}

fn play_stream_with_ui(
    mpv_path: &Path,
    stream_url: &str,
    title: Option<&str>,
    recommendations: Option<RecommendationsReceiver>,
) -> Result<PlaybackOutcome> {
    let mut session = MpvSession::start(mpv_path, stream_url, title.is_none())?;
    let mut ui = PlaybackUi::new().context("failed to initialize terminal UI")?;
    let mut state = PlaybackState {
        title: title.unwrap_or("Loading stream...").to_string(),
        ..PlaybackState::default()
    };
    let mut up_next = UpNextState::new(recommendations);
    let mut finished_code: Option<u8> = None;
    let mut stop_requested = false;

    ui.render(PlaybackView::from_state(
        &state,
        up_next.status_line(),
        up_next.overlay_view(false),
    ))?;

    loop {
        up_next.poll_receiver();

        if let Some(code) = finished_code {
            if let Some(next_candidate) = up_next.next_after_finish() {
                return Ok(PlaybackOutcome::PlayNext(next_candidate.playback_input()));
            }

            if up_next.should_exit_after_finish() {
                return Ok(PlaybackOutcome::Finished(code));
            }
        } else {
            while let Ok(event) = session.try_recv_event() {
                match event {
                    PlayerEvent::PropertyChange { name, data } => {
                        apply_property_change(&mut state, &name, data)
                    }
                    PlayerEvent::EndFile { reason, error } => {
                        if let Some(error) = error.filter(|value| value != "success") {
                            bail!("mpv playback failed: {error}");
                        }

                        if matches!(reason.as_deref(), Some("error")) {
                            bail!("mpv playback ended with an error.");
                        }

                        let code = session.wait_for_exit()?;
                        finished_code = Some(code);
                        if code != 0 || stop_requested || matches!(reason.as_deref(), Some("quit"))
                        {
                            return Ok(PlaybackOutcome::Finished(code));
                        }
                        up_next.on_playback_finished();
                        break;
                    }
                    PlayerEvent::Shutdown | PlayerEvent::Disconnected => {
                        let code = session.wait_for_exit()?;
                        return Ok(PlaybackOutcome::Finished(code));
                    }
                }
            }

            if finished_code.is_none() {
                if let Some(code) = session.try_wait()? {
                    finished_code = Some(code);
                    if code != 0 || stop_requested {
                        return Ok(PlaybackOutcome::Finished(code));
                    }
                    up_next.on_playback_finished();
                }
            }
        }

        ui.render(PlaybackView::from_state(
            &state,
            up_next.status_line(),
            up_next.overlay_view(finished_code.is_some()),
        ))?;

        if let Some(control) = ui.poll_control()? {
            if finished_code.is_some() {
                match control {
                    Control::SelectUpNext(index) => {
                        if up_next.select(index, true) {
                            if let Some(next_candidate) = up_next.next_after_finish() {
                                return Ok(PlaybackOutcome::PlayNext(
                                    next_candidate.playback_input(),
                                ));
                            }
                        }
                    }
                    Control::ConfirmUpNext => {
                        if let Some(next_candidate) = up_next.next_after_finish() {
                            return Ok(PlaybackOutcome::PlayNext(next_candidate.playback_input()));
                        }
                    }
                    Control::Quit => {
                        return Ok(PlaybackOutcome::Finished(finished_code.unwrap_or(0)));
                    }
                    _ => {}
                }
            } else {
                match control {
                    Control::TogglePause if !up_next.overlay_visible() => session.toggle_pause()?,
                    Control::SeekBackward if !up_next.overlay_visible() => {
                        session.seek_relative(-SEEK_SECONDS)?
                    }
                    Control::SeekForward if !up_next.overlay_visible() => {
                        session.seek_relative(SEEK_SECONDS)?
                    }
                    Control::VolumeDown if !up_next.overlay_visible() => {
                        session.adjust_volume(-UI_VOLUME_STEP)?
                    }
                    Control::VolumeUp if !up_next.overlay_visible() => {
                        session.adjust_volume(UI_VOLUME_STEP)?
                    }
                    Control::ToggleMute if !up_next.overlay_visible() => session.toggle_mute()?,
                    Control::ToggleUpNext => up_next.toggle_overlay(),
                    Control::CloseOverlay => up_next.close_overlay(),
                    Control::SelectUpNext(index) => {
                        if up_next.select(index, true) {
                            up_next.close_overlay();
                        }
                    }
                    Control::ConfirmUpNext => {
                        if up_next.has_ready_candidates() {
                            up_next.mark_explicit();
                            up_next.close_overlay();
                        }
                    }
                    Control::Quit => {
                        stop_requested = true;
                        session.quit()?;
                    }
                    _ => {}
                }
            }
        }
    }
}

fn apply_property_change(state: &mut PlaybackState, name: &str, data: Value) {
    match name {
        "time-pos" => {
            if let Some(value) = data.as_f64() {
                state.time_pos = value;
            }
        }
        "duration" => {
            state.duration = data.as_f64();
        }
        "pause" => {
            if let Some(value) = data.as_bool() {
                state.paused = value;
            }
        }
        "volume" => {
            if let Some(value) = data.as_f64() {
                state.volume = value;
            }
        }
        "mute" => {
            if let Some(value) = data.as_bool() {
                state.muted = value;
            }
        }
        "media-title" => {
            if let Some(value) = data.as_str() {
                state.title = value.to_string();
            }
        }
        _ => {}
    }
}

fn exit_code(status: &ExitStatus) -> u8 {
    status
        .code()
        .map(|code| code.clamp(0, u8::MAX as i32) as u8)
        .unwrap_or(1)
}

struct MpvSession {
    child: Child,
    _socket_dir: TempDir,
    writer: UnixStream,
    events: Receiver<PlayerEvent>,
    volume: f64,
}

impl MpvSession {
    fn start(mpv_path: &Path, stream_url: &str, observe_title: bool) -> Result<Self> {
        let socket_dir = Builder::new()
            .prefix("ytplay-")
            .tempdir_in("/tmp")
            .context("failed to create temporary directory for mpv IPC")?;
        let socket_path = socket_dir.path().join("mpv.sock");

        let child = ipc_mpv_command(mpv_path, stream_url, &socket_path)
            .spawn()
            .with_context(|| format!("failed to start `{}`", mpv_path.display()))?;

        let writer = connect_to_socket(&socket_path)?;
        let reader = writer
            .try_clone()
            .context("failed to clone mpv IPC socket")?;
        let events = spawn_event_reader(reader);

        let mut session = Self {
            child,
            _socket_dir: socket_dir,
            writer,
            events,
            volume: 100.0,
        };

        session.observe_property(1, "time-pos")?;
        session.observe_property(2, "duration")?;
        session.observe_property(3, "pause")?;
        session.observe_property(4, "volume")?;
        session.observe_property(5, "mute")?;
        session.request_property(101, "volume")?;
        session.request_property(102, "pause")?;
        session.request_property(103, "mute")?;
        session.request_property(104, "duration")?;
        if observe_title {
            session.observe_property(6, "media-title")?;
            session.request_property(105, "media-title")?;
        }

        Ok(session)
    }

    fn try_recv_event(&mut self) -> std::result::Result<PlayerEvent, mpsc::TryRecvError> {
        match self.events.try_recv() {
            Ok(PlayerEvent::PropertyChange { name, data }) => {
                if name == "volume" {
                    if let Some(value) = data.as_f64() {
                        self.volume = value;
                    }
                }

                Ok(PlayerEvent::PropertyChange { name, data })
            }
            other => other,
        }
    }

    fn try_wait(&mut self) -> Result<Option<u8>> {
        self.child
            .try_wait()
            .context("failed to poll mpv process state")
            .map(|status| status.map(|status| exit_code(&status)))
    }

    fn wait_for_exit(&mut self) -> Result<u8> {
        self.child
            .wait()
            .context("failed to wait for mpv process to exit")
            .map(|status| exit_code(&status))
    }

    fn toggle_pause(&mut self) -> Result<()> {
        self.send_command(json!({ "command": ["cycle", "pause"] }))
    }

    fn seek_relative(&mut self, seconds: i64) -> Result<()> {
        self.send_command(json!({ "command": ["seek", seconds, "relative"] }))
    }

    fn adjust_volume(&mut self, delta: f64) -> Result<()> {
        let next_volume = mpv_volume_from_ui(ui_volume_percent(self.volume) + delta);
        self.send_command(json!({ "command": ["set_property", "volume", next_volume] }))
    }

    fn toggle_mute(&mut self) -> Result<()> {
        self.send_command(json!({ "command": ["cycle", "mute"] }))
    }

    fn quit(&mut self) -> Result<()> {
        self.send_command(json!({ "command": ["quit"] }))
    }

    fn observe_property(&mut self, id: u64, property: &str) -> Result<()> {
        self.send_command(json!({ "command": ["observe_property", id, property] }))
    }

    fn request_property(&mut self, request_id: u64, property: &str) -> Result<()> {
        self.send_command(json!({
            "command": ["get_property", property],
            "request_id": request_id,
        }))
    }

    fn send_command(&mut self, command: Value) -> Result<()> {
        serde_json::to_writer(&mut self.writer, &command)
            .context("failed to write mpv IPC command")?;
        self.writer
            .write_all(b"\n")
            .context("failed to terminate mpv IPC command")?;
        self.writer
            .flush()
            .context("failed to flush mpv IPC command")
    }
}

impl Drop for MpvSession {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn ipc_mpv_command(program: &Path, stream_url: &str, socket_path: &Path) -> Command {
    let mut command = mpv_command(program, stream_url);
    command.args([
        "--input-terminal=no",
        "--terminal=no",
        "--force-window=no",
        &format!("--input-ipc-server={}", socket_path.display()),
    ]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn connect_to_socket(socket_path: &Path) -> Result<UnixStream> {
    let start = Instant::now();

    loop {
        match UnixStream::connect(socket_path) {
            Ok(stream) => return Ok(stream),
            Err(_) if start.elapsed() < SOCKET_WAIT_TIMEOUT => thread::sleep(SOCKET_POLL_INTERVAL),
            Err(error) => {
                return Err(anyhow!(
                    "failed to connect to mpv IPC socket at {}: {error}",
                    socket_path.display()
                ));
            }
        }
    }
}

fn spawn_event_reader(stream: UnixStream) -> Receiver<PlayerEvent> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let reader = BufReader::new(stream);

        for line in reader.lines() {
            let Ok(line) = line else {
                break;
            };

            if let Some(event) = parse_player_event(&line) {
                if tx.send(event).is_err() {
                    break;
                }
            }
        }

        let _ = tx.send(PlayerEvent::Disconnected);
    });

    rx
}

fn parse_player_event(line: &str) -> Option<PlayerEvent> {
    let payload: Value = serde_json::from_str(line).ok()?;

    if let Some(event_name) = payload.get("event").and_then(Value::as_str) {
        return match event_name {
            "property-change" => Some(PlayerEvent::PropertyChange {
                name: payload.get("name")?.as_str()?.to_string(),
                data: payload.get("data").cloned().unwrap_or(Value::Null),
            }),
            "end-file" => Some(PlayerEvent::EndFile {
                reason: payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                error: payload
                    .get("error")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            }),
            "shutdown" => Some(PlayerEvent::Shutdown),
            _ => None,
        };
    }

    let property_name = payload
        .get("request_id")
        .and_then(Value::as_u64)
        .and_then(initial_request_name)?;
    let data = payload.get("data")?.clone();

    Some(PlayerEvent::PropertyChange {
        name: property_name.to_string(),
        data,
    })
}

fn initial_request_name(request_id: u64) -> Option<&'static str> {
    match request_id {
        101 => Some("volume"),
        102 => Some("pause"),
        103 => Some("mute"),
        104 => Some("duration"),
        105 => Some("media-title"),
        _ => None,
    }
}

pub(crate) fn ui_volume_percent(mpv_volume: f64) -> f64 {
    (mpv_volume / MPV_VOLUME_MULTIPLIER).clamp(0.0, 100.0)
}

fn mpv_volume_from_ui(ui_volume: f64) -> f64 {
    (ui_volume.clamp(0.0, 100.0) * MPV_VOLUME_MULTIPLIER).clamp(0.0, 200.0)
}

struct UpNextState {
    source: RecommendationsState,
    overlay_visible: bool,
    selected_index: usize,
    explicit_selection: bool,
    autoplay_deadline: Option<Instant>,
}

enum RecommendationsState {
    Disabled,
    Loading(RecommendationsReceiver),
    Ready(Vec<UpNextCandidate>),
    Failed(String),
}

impl UpNextState {
    fn new(receiver: Option<RecommendationsReceiver>) -> Self {
        let source = receiver
            .map(RecommendationsState::Loading)
            .unwrap_or(RecommendationsState::Disabled);

        Self {
            source,
            overlay_visible: false,
            selected_index: 0,
            explicit_selection: false,
            autoplay_deadline: None,
        }
    }

    fn poll_receiver(&mut self) {
        let RecommendationsState::Loading(receiver) = &self.source else {
            return;
        };

        match receiver.try_recv() {
            Ok(Ok(candidates)) => {
                self.source = RecommendationsState::Ready(candidates);
                self.selected_index = 0;
            }
            Ok(Err(error)) => {
                self.source = RecommendationsState::Failed(error.to_string());
            }
            Err(TryRecvError::Disconnected) => {
                self.source = RecommendationsState::Failed(
                    "recommendation lookup stopped unexpectedly".to_string(),
                );
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    fn overlay_visible(&self) -> bool {
        self.overlay_visible
    }

    fn toggle_overlay(&mut self) {
        self.overlay_visible = !self.overlay_visible;
    }

    fn close_overlay(&mut self) {
        self.overlay_visible = false;
    }

    fn has_ready_candidates(&self) -> bool {
        matches!(&self.source, RecommendationsState::Ready(candidates) if !candidates.is_empty())
    }

    fn mark_explicit(&mut self) {
        self.explicit_selection = true;
    }

    fn select(&mut self, index: usize, explicit: bool) -> bool {
        let RecommendationsState::Ready(candidates) = &self.source else {
            return false;
        };

        if index >= candidates.len() {
            return false;
        }

        self.selected_index = index;
        if explicit {
            self.explicit_selection = true;
        }
        true
    }

    fn selected_candidate(&self) -> Option<UpNextCandidate> {
        let RecommendationsState::Ready(candidates) = &self.source else {
            return None;
        };

        candidates.get(self.selected_index).cloned()
    }

    fn on_playback_finished(&mut self) {
        self.overlay_visible = true;

        if !self.explicit_selection {
            self.autoplay_deadline = Some(Instant::now() + UP_NEXT_AUTOPLAY_DELAY);
        }
    }

    fn next_after_finish(&mut self) -> Option<UpNextCandidate> {
        if self.explicit_selection {
            return self.selected_candidate();
        }

        let deadline = self.autoplay_deadline?;
        if Instant::now() >= deadline {
            return self.selected_candidate();
        }

        None
    }

    fn should_exit_after_finish(&self) -> bool {
        match &self.source {
            RecommendationsState::Disabled => true,
            RecommendationsState::Loading(_) => false,
            RecommendationsState::Ready(candidates) => candidates.is_empty(),
            RecommendationsState::Failed(_) => true,
        }
    }

    fn status_line(&self) -> Option<String> {
        match &self.source {
            RecommendationsState::Disabled => None,
            RecommendationsState::Loading(_) => {
                Some("Up Next: loading recommendations...".to_string())
            }
            RecommendationsState::Ready(candidates) if !candidates.is_empty() => Some(format!(
                "Up Next: {}",
                candidates[self.selected_index].display_label()
            )),
            RecommendationsState::Ready(_) => Some("Up Next: no recommendations found".to_string()),
            RecommendationsState::Failed(_) => Some("Up Next: unavailable".to_string()),
        }
    }

    fn overlay_view(&self, playback_finished: bool) -> Option<UpNextOverlayView> {
        if !self.overlay_visible {
            return None;
        }

        match &self.source {
            RecommendationsState::Loading(_) => Some(UpNextOverlayView {
                heading: "Up Next".to_string(),
                message: if playback_finished {
                    "Loading recommendations before autoplay...".to_string()
                } else {
                    "Loading recommendations...".to_string()
                },
                items: Vec::new(),
                help_lines: if playback_finished {
                    vec!["Q stop".to_string()]
                } else {
                    vec!["N or Esc close".to_string()]
                },
            }),
            RecommendationsState::Failed(error) => Some(UpNextOverlayView {
                heading: "Up Next".to_string(),
                message: fit_overlay_error(error),
                items: Vec::new(),
                help_lines: if playback_finished {
                    vec!["Q stop".to_string()]
                } else {
                    vec!["N or Esc close".to_string()]
                },
            }),
            RecommendationsState::Ready(candidates) if !candidates.is_empty() => {
                let message = if playback_finished {
                    if self.explicit_selection {
                        "Queued selection will start now.".to_string()
                    } else {
                        let seconds_left = self
                            .autoplay_deadline
                            .map(|deadline| {
                                deadline.saturating_duration_since(Instant::now()).as_secs()
                            })
                            .unwrap_or(0);
                        format!(
                            "Autoplaying {} in {}s. Press 1-5 to choose or Enter to play now.",
                            self.selected_index + 1,
                            seconds_left
                        )
                    }
                } else {
                    "Press 1-5 to queue the next video.".to_string()
                };

                let items = candidates
                    .iter()
                    .enumerate()
                    .map(|(index, candidate)| {
                        let marker = if index == self.selected_index {
                            '>'
                        } else {
                            ' '
                        };
                        format!("{marker} {}. {}", index + 1, candidate.display_label())
                    })
                    .collect();

                let help_lines = if playback_finished {
                    vec!["1-5 choose   Enter play   Q stop".to_string()]
                } else {
                    vec!["1-5 queue   Enter keep selected   N or Esc close".to_string()]
                };

                Some(UpNextOverlayView {
                    heading: "Up Next".to_string(),
                    message,
                    items,
                    help_lines,
                })
            }
            RecommendationsState::Ready(_) => Some(UpNextOverlayView {
                heading: "Up Next".to_string(),
                message: "No recommendations were found for this track.".to_string(),
                items: Vec::new(),
                help_lines: if playback_finished {
                    vec!["Q stop".to_string()]
                } else {
                    vec!["N or Esc close".to_string()]
                },
            }),
            RecommendationsState::Disabled => None,
        }
    }
}

fn fit_overlay_error(message: &str) -> String {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        "Recommendations are unavailable.".to_string()
    } else {
        format!("Recommendations unavailable: {trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn parses_property_change_event() {
        let event = parse_player_event(r#"{"event":"property-change","name":"pause","data":true}"#)
            .unwrap();

        match event {
            PlayerEvent::PropertyChange { name, data } => {
                assert_eq!(name, "pause");
                assert_eq!(data, Value::Bool(true));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn parses_get_property_response_into_property_change() {
        let event =
            parse_player_event(r#"{"request_id":101,"error":"success","data":55}"#).unwrap();

        match event {
            PlayerEvent::PropertyChange { name, data } => {
                assert_eq!(name, "volume");
                assert_eq!(data, json!(55));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn builds_ipc_mpv_command_with_terminal_flags() {
        let command = ipc_mpv_command(
            Path::new("/bin/mpv"),
            "https://stream.example/audio",
            Path::new("/tmp/ytplay-test.sock"),
        );

        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert!(args.contains(&"--input-terminal=no".to_string()));
        assert!(args.contains(&"--terminal=no".to_string()));
        assert!(args.contains(&"--force-window=no".to_string()));
        assert!(args.contains(&"--input-ipc-server=/tmp/ytplay-test.sock".to_string()));
    }

    #[test]
    fn maps_default_mpv_volume_to_fifty_percent_ui() {
        assert_eq!(ui_volume_percent(100.0), 50.0);
    }

    #[test]
    fn caps_ui_volume_range_before_mapping_back_to_mpv() {
        assert_eq!(mpv_volume_from_ui(-5.0), 0.0);
        assert_eq!(mpv_volume_from_ui(100.0), 200.0);
        assert_eq!(mpv_volume_from_ui(150.0), 200.0);
    }

    #[test]
    fn up_next_defaults_to_first_candidate_after_countdown() {
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(vec![
            UpNextCandidate {
                video_id: "one".to_string(),
                title: "One".to_string(),
                uploader: Some("Artist".to_string()),
            },
            UpNextCandidate {
                video_id: "two".to_string(),
                title: "Two".to_string(),
                uploader: Some("Artist".to_string()),
            },
        ]))
        .unwrap();

        let mut up_next = UpNextState::new(Some(rx));
        up_next.poll_receiver();
        up_next.on_playback_finished();
        up_next.autoplay_deadline = Some(Instant::now());

        let candidate = up_next.next_after_finish().unwrap();
        assert_eq!(candidate.video_id, "one");
    }

    #[test]
    fn up_next_uses_explicit_selection_immediately() {
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(vec![
            UpNextCandidate {
                video_id: "one".to_string(),
                title: "One".to_string(),
                uploader: Some("Artist".to_string()),
            },
            UpNextCandidate {
                video_id: "two".to_string(),
                title: "Two".to_string(),
                uploader: Some("Artist".to_string()),
            },
        ]))
        .unwrap();

        let mut up_next = UpNextState::new(Some(rx));
        up_next.poll_receiver();
        assert!(up_next.select(1, true));
        up_next.on_playback_finished();

        let candidate = up_next.next_after_finish().unwrap();
        assert_eq!(candidate.video_id, "two");
    }
}
