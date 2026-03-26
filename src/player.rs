use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tempfile::{Builder, TempDir};

use crate::app::mpv_command;
use crate::ui::{PlaybackUi, PlaybackView};

const SOCKET_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(50);
const UI_VOLUME_STEP: f64 = 5.0;
const MPV_VOLUME_MULTIPLIER: f64 = 2.0;

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
    VolumeDown,
    VolumeUp,
    ToggleMute,
    Quit,
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
) -> Result<u8> {
    if use_terminal_ui {
        return play_stream_with_ui(mpv_path, stream_url, title);
    }

    play_stream_simple(mpv_path, stream_url)
}

fn play_stream_simple(mpv_path: &Path, stream_url: &str) -> Result<u8> {
    let status = mpv_command(mpv_path, stream_url)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to start `{}`", mpv_path.display()))?;

    Ok(exit_code(&status))
}

fn play_stream_with_ui(mpv_path: &Path, stream_url: &str, title: Option<&str>) -> Result<u8> {
    let mut session = MpvSession::start(mpv_path, stream_url, title.is_none())?;
    let mut ui = PlaybackUi::new().context("failed to initialize terminal UI")?;
    let mut state = PlaybackState {
        title: title.unwrap_or("Loading stream...").to_string(),
        ..PlaybackState::default()
    };

    ui.render(PlaybackView::from_state(&state))?;

    loop {
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

                    return session.wait_for_exit();
                }
                PlayerEvent::Shutdown | PlayerEvent::Disconnected => {
                    return session.wait_for_exit();
                }
            }
        }

        if let Some(code) = session.try_wait()? {
            return Ok(code);
        }

        ui.render(PlaybackView::from_state(&state))?;

        if let Some(control) = ui.poll_control()? {
            match control {
                Control::TogglePause => session.toggle_pause()?,
                Control::VolumeDown => session.adjust_volume(-UI_VOLUME_STEP)?,
                Control::VolumeUp => session.adjust_volume(UI_VOLUME_STEP)?,
                Control::ToggleMute => session.toggle_mute()?,
                Control::Quit => session.quit()?,
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
