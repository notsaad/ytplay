use std::io::{self, Write};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::queue;
use crossterm::style::Print;
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};

use crate::player::{Control, PlaybackState};

const CONTROL_HINTS: [(&str, &str); 5] = [
    ("Play/Pause", "P"),
    ("Volume -", "U"),
    ("Volume +", "I"),
    ("Mute", "M"),
    ("Quit", "Q"),
];

pub(crate) struct PlaybackUi {
    stdout: io::Stdout,
}

pub(crate) struct PlaybackView<'a> {
    title: &'a str,
    status: &'a str,
    volume: String,
    elapsed: String,
    total: String,
    progress_ratio: f64,
}

impl PlaybackUi {
    pub(crate) fn new() -> Result<Self> {
        terminal::enable_raw_mode().context("failed to enable raw mode")?;

        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            Hide,
            Clear(ClearType::All),
            MoveTo(0, 0)
        )
        .context("failed to clear terminal")?;

        Ok(Self { stdout })
    }

    pub(crate) fn render(&mut self, view: PlaybackView<'_>) -> Result<()> {
        let (columns, rows) = terminal::size().unwrap_or((100, 24));
        let content_width = columns.saturating_sub(4).clamp(36, 96);
        let left = columns.saturating_sub(content_width) / 2;
        let control_rows = control_rows_for_width(content_width);
        let layout_height = 7 + (control_rows.len() as u16 * 2);
        let top = rows.saturating_sub(layout_height) / 2;

        execute!(self.stdout, MoveTo(0, 0), Clear(ClearType::All))
            .context("failed to draw terminal UI")?;

        draw_centered_line(&mut self.stdout, left, top, content_width, view.title)?;

        let bar_width = progress_bar_width(content_width);
        let bar = progress_bar(view.progress_ratio, bar_width);
        draw_centered_line(&mut self.stdout, left, top + 2, content_width, &bar)?;

        let timing_line = format!("{} / {}", view.elapsed, view.total);
        draw_centered_line(&mut self.stdout, left, top + 3, content_width, &timing_line)?;

        let status_line = format!("Status: {}  Volume: {}", view.status, view.volume);
        draw_centered_line(&mut self.stdout, left, top + 5, content_width, &status_line)?;

        let mut row_y = top + 7;
        for row in control_rows {
            draw_control_row(&mut self.stdout, left, row_y, content_width, row)?;
            row_y += 2;
        }

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
        let _ = execute!(self.stdout, Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

impl<'a> PlaybackView<'a> {
    pub(crate) fn from_state(state: &'a PlaybackState) -> Self {
        let status = if state.paused { "Paused" } else { "Playing" };
        let volume = if state.muted {
            format!(
                "{:.0}% (muted)",
                crate::player::ui_volume_percent(state.volume)
            )
        } else {
            format!("{:.0}%", crate::player::ui_volume_percent(state.volume))
        };
        let elapsed = format_timestamp(state.time_pos);
        let total = state
            .duration
            .map(format_timestamp)
            .unwrap_or_else(|| "--:--".to_string());
        let progress_ratio = state
            .duration
            .filter(|duration| *duration > 0.0)
            .map(|duration| (state.time_pos / duration).clamp(0.0, 1.0))
            .unwrap_or(0.0);

        Self {
            title: state.title.as_str(),
            status,
            volume,
            elapsed,
            total,
            progress_ratio,
        }
    }
}

fn draw_centered_line(
    stdout: &mut io::Stdout,
    left: u16,
    y: u16,
    width: u16,
    text: &str,
) -> Result<()> {
    let fitted = fit_text(text, width as usize);
    let offset = centered_offset(width as usize, fitted.chars().count());
    queue!(stdout, MoveTo(left + offset as u16, y), Print(fitted))
        .context("failed to queue centered line")?;
    Ok(())
}

fn draw_control_row(
    stdout: &mut io::Stdout,
    left: u16,
    y: u16,
    width: u16,
    items: &[(&str, &str)],
) -> Result<()> {
    let cell_width = (width as usize / items.len()).max(10);

    for (index, (label, key)) in items.iter().enumerate() {
        let cell_left = left as usize + (index * cell_width);
        let label_offset = centered_offset(cell_width, label.chars().count());
        let key_offset = centered_offset(cell_width, key.chars().count());

        queue!(
            stdout,
            MoveTo((cell_left + label_offset) as u16, y),
            Print(*label),
            MoveTo((cell_left + key_offset) as u16, y + 1),
            Print(*key)
        )
        .context("failed to queue control row")?;
    }

    Ok(())
}

fn control_rows_for_width(width: u16) -> Vec<&'static [(&'static str, &'static str)]> {
    if width >= 70 {
        vec![&CONTROL_HINTS]
    } else if width >= 48 {
        vec![&CONTROL_HINTS[..3], &CONTROL_HINTS[3..]]
    } else {
        CONTROL_HINTS.iter().map(std::slice::from_ref).collect()
    }
}

fn progress_bar_width(content_width: u16) -> usize {
    content_width.saturating_sub(8).clamp(12, 72) as usize
}

fn progress_bar(progress_ratio: f64, width: usize) -> String {
    let filled = (progress_ratio.clamp(0.0, 1.0) * width as f64).round() as usize;
    let filled = filled.min(width);
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

fn fit_text(text: &str, max_width: usize) -> String {
    if text.chars().count() <= max_width {
        return text.to_string();
    }

    if max_width <= 3 {
        return text.chars().take(max_width).collect();
    }

    let mut truncated: String = text.chars().take(max_width - 3).collect();
    truncated.push_str("...");
    truncated
}

fn centered_offset(width: usize, text_width: usize) -> usize {
    width.saturating_sub(text_width) / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_progress_bar_with_known_duration() {
        let rendered = progress_bar(0.25, 20);
        assert_eq!(rendered, "[#####---------------]");
    }

    #[test]
    fn formats_progress_bar_without_duration() {
        let rendered = progress_bar(0.0, 10);
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
            volume: 100.0,
            muted: true,
        };

        let view = PlaybackView::from_state(&state);
        assert_eq!(view.title, "Test Track");
        assert_eq!(view.status, "Paused");
        assert_eq!(view.volume, "50% (muted)");
        assert_eq!(view.elapsed, "00:15");
        assert_eq!(view.total, "01:00");
        assert!((view.progress_ratio - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn chooses_multiple_control_rows_for_narrow_terminals() {
        let rows = control_rows_for_width(50);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].len(), 3);
        assert_eq!(rows[1].len(), 2);
    }

    #[test]
    fn truncates_long_text_with_ascii_ellipsis() {
        assert_eq!(fit_text("abcdefghijklmnopqrstuvwxyz", 10), "abcdefg...");
    }
}
