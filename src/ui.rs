use std::io::{self, Write};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{self, Clear, ClearType};

use crate::player::{Control, PlaybackState};

pub(crate) struct PlaybackUi {
    stdout: io::Stdout,
}

pub(crate) struct PlaybackView<'a> {
    title: &'a str,
    status: &'a str,
    volume: String,
    progress_line: String,
}

impl PlaybackUi {
    pub(crate) fn new() -> Result<Self> {
        terminal::enable_raw_mode().context("failed to enable raw mode")?;

        let mut stdout = io::stdout();
        execute!(stdout, Hide, Clear(ClearType::All), MoveTo(0, 0))
            .context("failed to clear terminal")?;

        Ok(Self { stdout })
    }

    pub(crate) fn render(&mut self, view: PlaybackView<'_>) -> Result<()> {
        execute!(self.stdout, MoveTo(0, 0), Clear(ClearType::FromCursorDown))
            .context("failed to draw terminal UI")?;

        writeln!(self.stdout, "{}", truncate_line(view.title))?;
        writeln!(self.stdout, "{}", truncate_line(&view.progress_line))?;
        writeln!(
            self.stdout,
            "{}",
            truncate_line(&format!("Status: {}  Volume: {}", view.status, view.volume))
        )?;
        writeln!(self.stdout)?;
        writeln!(
            self.stdout,
            " Play/Pause   Volume -   Volume +   Mute   Quit"
        )?;
        writeln!(self.stdout, "     P           U          I        M      Q")?;
        self.stdout.flush().context("failed to flush terminal UI")
    }

    pub(crate) fn poll_control(&mut self) -> Result<Option<Control>> {
        if !event::poll(Duration::from_millis(120)).context("failed to poll terminal input")? {
            return Ok(None);
        }

        let Event::Key(key) = event::read().context("failed to read terminal input")? else {
            return Ok(None);
        };

        if key.kind != KeyEventKind::Press {
            return Ok(None);
        }

        let control = match key.code {
            KeyCode::Char('p') | KeyCode::Char('P') => Some(Control::TogglePause),
            KeyCode::Char('u') | KeyCode::Char('U') => Some(Control::VolumeDown),
            KeyCode::Char('i') | KeyCode::Char('I') => Some(Control::VolumeUp),
            KeyCode::Char('m') | KeyCode::Char('M') => Some(Control::ToggleMute),
            KeyCode::Char('q') | KeyCode::Char('Q') => Some(Control::Quit),
            _ => None,
        };

        Ok(control)
    }
}

impl Drop for PlaybackUi {
    fn drop(&mut self) {
        let _ = execute!(self.stdout, Show, MoveTo(0, 6));
        let _ = terminal::disable_raw_mode();
    }
}

impl<'a> PlaybackView<'a> {
    pub(crate) fn from_state(state: &'a PlaybackState) -> Self {
        let status = if state.paused { "Paused" } else { "Playing" };
        let volume = if state.muted {
            format!("{:.0}% (muted)", state.volume)
        } else {
            format!("{:.0}%", state.volume)
        };
        let progress_line = format_progress_line(state.time_pos, state.duration);

        Self {
            title: state.title.as_str(),
            status,
            volume,
            progress_line,
        }
    }
}

fn format_progress_line(time_pos: f64, duration: Option<f64>) -> String {
    let width = progress_width().unwrap_or(32);
    let bar = progress_bar(time_pos, duration, width);

    match duration {
        Some(duration) => format!(
            "{} {} / {}",
            bar,
            format_timestamp(time_pos),
            format_timestamp(duration)
        ),
        None => format!("{} {}", bar, format_timestamp(time_pos)),
    }
}

fn progress_width() -> Option<usize> {
    terminal::size()
        .ok()
        .map(|(columns, _)| columns.saturating_sub(24).clamp(20, 50) as usize)
}

fn progress_bar(time_pos: f64, duration: Option<f64>, width: usize) -> String {
    let filled = duration
        .filter(|duration| *duration > 0.0)
        .map(|duration| ((time_pos / duration).clamp(0.0, 1.0) * width as f64).round() as usize)
        .unwrap_or(0)
        .min(width);

    let empty = width.saturating_sub(filled);
    format!("[{}{}]", "#".repeat(filled), "-".repeat(empty))
}

fn format_timestamp(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let secs = total % 60;

    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes:02}:{secs:02}")
    }
}

fn truncate_line(line: &str) -> String {
    let max_width = terminal::size()
        .ok()
        .map(|(columns, _)| columns as usize)
        .unwrap_or(80);

    line.chars().take(max_width.saturating_sub(1)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_progress_bar_with_known_duration() {
        let rendered = progress_bar(25.0, Some(100.0), 20);
        assert_eq!(rendered, "[#####---------------]");
    }

    #[test]
    fn formats_progress_bar_without_duration() {
        let rendered = progress_bar(25.0, None, 10);
        assert_eq!(rendered, "[----------]");
    }

    #[test]
    fn formats_timestamps_for_minutes_and_hours() {
        assert_eq!(format_timestamp(65.0), "01:05");
        assert_eq!(format_timestamp(3723.0), "01:02:03");
    }

    #[test]
    fn builds_playback_view_strings() {
        let state = PlaybackState {
            title: "Test Track".to_string(),
            time_pos: 15.0,
            duration: Some(60.0),
            paused: true,
            volume: 80.0,
            muted: true,
        };

        let view = PlaybackView::from_state(&state);
        assert_eq!(view.title, "Test Track");
        assert_eq!(view.status, "Paused");
        assert_eq!(view.volume, "80% (muted)");
        assert!(view.progress_line.contains("00:15 / 01:00"));
    }
}
