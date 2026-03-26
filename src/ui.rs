use std::io::{self, Write};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::queue;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};

use crate::player::{Control, PlaybackState};

const PLAYBACK_CONTROL_HINTS: [(&str, &str); 8] = [
    ("Play/Pause", "P"),
    ("Back 30", "J"),
    ("Fwd 30", "L"),
    ("Volume -", "U"),
    ("Volume +", "I"),
    ("Mute", "M"),
    ("Up Next", "N"),
    ("Quit", "Q"),
];

pub(crate) struct PlaybackUi {
    stdout: io::Stdout,
}

pub(crate) struct PlaybackView<'a> {
    title: String,
    status: String,
    volume: String,
    elapsed: String,
    total: String,
    progress_ratio: f64,
    queue_status: Option<String>,
    overlay: Option<UpNextOverlayView>,
    _marker: std::marker::PhantomData<&'a ()>,
}

pub(crate) struct UpNextOverlayView {
    pub(crate) heading: String,
    pub(crate) message: String,
    pub(crate) items: Vec<OverlayItem>,
    pub(crate) help_lines: Vec<String>,
}

pub(crate) struct OverlayItem {
    pub(crate) text: String,
    pub(crate) selected: bool,
}

struct DisplayLine {
    text: String,
    style: LineStyle,
}

#[derive(Clone, Copy)]
enum LineStyle {
    Normal,
    Selected,
}

impl DisplayLine {
    fn normal(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: LineStyle::Normal,
        }
    }

    fn selected(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: LineStyle::Selected,
        }
    }

    fn blank() -> Self {
        Self::normal(String::new())
    }
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
        let content_lines = compose_lines(&view, content_width as usize);
        let layout_height = content_lines.len() as u16;
        let top = rows.saturating_sub(layout_height) / 2;

        execute!(self.stdout, MoveTo(0, 0), Clear(ClearType::All))
            .context("failed to draw terminal UI")?;

        for (index, line) in content_lines.iter().enumerate() {
            draw_centered_line(
                &mut self.stdout,
                left,
                top + index as u16,
                content_width,
                line,
            )?;
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
            KeyCode::Char('j') | KeyCode::Char('J') => Some(Control::SeekBackward),
            KeyCode::Char('l') | KeyCode::Char('L') => Some(Control::SeekForward),
            KeyCode::Char('u') | KeyCode::Char('U') => Some(Control::VolumeDown),
            KeyCode::Char('i') | KeyCode::Char('I') => Some(Control::VolumeUp),
            KeyCode::Char('m') | KeyCode::Char('M') => Some(Control::ToggleMute),
            KeyCode::Char('n') | KeyCode::Char('N') => Some(Control::ToggleUpNext),
            KeyCode::Up | KeyCode::Left => Some(Control::MoveUpNext(-1)),
            KeyCode::Down | KeyCode::Right => Some(Control::MoveUpNext(1)),
            KeyCode::Char('1') => Some(Control::SelectUpNext(0)),
            KeyCode::Char('2') => Some(Control::SelectUpNext(1)),
            KeyCode::Char('3') => Some(Control::SelectUpNext(2)),
            KeyCode::Char('4') => Some(Control::SelectUpNext(3)),
            KeyCode::Char('5') => Some(Control::SelectUpNext(4)),
            KeyCode::Enter => Some(Control::ConfirmUpNext),
            KeyCode::Esc => Some(Control::CloseOverlay),
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
    pub(crate) fn from_state(
        state: &'a PlaybackState,
        queue_status: Option<String>,
        overlay: Option<UpNextOverlayView>,
    ) -> Self {
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
            title: state.title.clone(),
            status: status.to_string(),
            volume,
            elapsed,
            total,
            progress_ratio,
            queue_status,
            overlay,
            _marker: std::marker::PhantomData,
        }
    }
}

fn compose_lines(view: &PlaybackView<'_>, content_width: usize) -> Vec<DisplayLine> {
    let mut lines = Vec::new();
    let bar_width = progress_bar_width(content_width as u16);
    let bar = progress_bar(view.progress_ratio, bar_width);

    lines.push(DisplayLine::normal(view.title.clone()));
    lines.push(DisplayLine::blank());
    lines.push(DisplayLine::normal(bar));
    lines.push(DisplayLine::normal(format!(
        "{} / {}",
        view.elapsed, view.total
    )));
    lines.push(DisplayLine::blank());
    lines.push(DisplayLine::normal(format!(
        "Status: {}  Volume: {}",
        view.status, view.volume
    )));

    if let Some(queue_status) = &view.queue_status {
        lines.push(DisplayLine::normal(queue_status.clone()));
    }

    lines.push(DisplayLine::blank());

    if let Some(overlay) = &view.overlay {
        lines.push(DisplayLine::normal(overlay.heading.clone()));
        lines.push(DisplayLine::normal(overlay.message.clone()));
        if !overlay.items.is_empty() {
            lines.push(DisplayLine::blank());
            lines.extend(overlay.items.iter().map(|item| {
                if item.selected {
                    DisplayLine::selected(item.text.clone())
                } else {
                    DisplayLine::normal(item.text.clone())
                }
            }));
        }
        if !overlay.help_lines.is_empty() {
            lines.push(DisplayLine::blank());
            lines.extend(overlay.help_lines.iter().cloned().map(DisplayLine::normal));
        }
    } else {
        lines.extend(
            control_rows_for_width(content_width as u16, &PLAYBACK_CONTROL_HINTS)
                .into_iter()
                .map(DisplayLine::normal),
        );
    }

    lines
}

fn draw_centered_line(
    stdout: &mut io::Stdout,
    left: u16,
    y: u16,
    width: u16,
    line: &DisplayLine,
) -> Result<()> {
    let fitted = fit_text(&line.text, width as usize);
    let offset = centered_offset(width as usize, fitted.chars().count());

    match line.style {
        LineStyle::Normal => {
            queue!(
                stdout,
                ResetColor,
                MoveTo(left + offset as u16, y),
                Print(fitted)
            )
            .context("failed to queue centered line")?;
        }
        LineStyle::Selected => {
            queue!(
                stdout,
                SetForegroundColor(Color::Cyan),
                MoveTo(left + offset as u16, y),
                Print(fitted),
                ResetColor
            )
            .context("failed to queue styled centered line")?;
        }
    }

    Ok(())
}

fn control_rows_for_width(width: u16, controls: &[(&str, &str)]) -> Vec<String> {
    let per_row = if width >= 88 {
        4
    } else if width >= 64 {
        3
    } else if width >= 44 {
        2
    } else {
        1
    };

    let mut rows = Vec::new();

    for chunk in controls.chunks(per_row) {
        let cell_width = (width as usize / chunk.len()).max(12);
        rows.push(format_control_row(chunk, cell_width, true));
        rows.push(format_control_row(chunk, cell_width, false));
    }

    rows
}

fn format_control_row(items: &[(&str, &str)], cell_width: usize, label_row: bool) -> String {
    let mut row = String::new();

    for (label, key) in items {
        let text = if label_row { *label } else { *key };
        row.push_str(&centered_cell(text, cell_width));
    }

    row.trim_end().to_string()
}

fn centered_cell(text: &str, width: usize) -> String {
    let fitted = fit_text(text, width);
    let text_width = fitted.chars().count();
    let left_padding = centered_offset(width, text_width);
    let right_padding = width.saturating_sub(left_padding + text_width);
    format!(
        "{}{}{}",
        " ".repeat(left_padding),
        fitted,
        " ".repeat(right_padding)
    )
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

        let view =
            PlaybackView::from_state(&state, Some("Up Next: Another Song".to_string()), None);
        assert_eq!(view.title, "Test Track");
        assert_eq!(view.status, "Paused");
        assert_eq!(view.volume, "50% (muted)");
        assert_eq!(view.elapsed, "00:15");
        assert_eq!(view.total, "01:00");
        assert!((view.progress_ratio - 0.25).abs() < f64::EPSILON);
        assert_eq!(view.queue_status.as_deref(), Some("Up Next: Another Song"));
    }

    #[test]
    fn chooses_multiple_control_rows_for_narrow_terminals() {
        let rows = control_rows_for_width(50, &PLAYBACK_CONTROL_HINTS);
        assert_eq!(rows.len(), 8);
    }

    #[test]
    fn truncates_long_text_with_ascii_ellipsis() {
        assert_eq!(fit_text("abcdefghijklmnopqrstuvwxyz", 10), "abcdefg...");
    }

    #[test]
    fn composes_overlay_lines() {
        let state = PlaybackState {
            title: "Test Track".to_string(),
            time_pos: 10.0,
            duration: Some(60.0),
            paused: false,
            volume: 100.0,
            muted: false,
        };
        let view = PlaybackView::from_state(
            &state,
            Some("Up Next: Song B".to_string()),
            Some(UpNextOverlayView {
                heading: "Up Next".to_string(),
                message: "Autoplaying 1 in 8s".to_string(),
                items: vec![
                    OverlayItem {
                        text: "1. Song B".to_string(),
                        selected: true,
                    },
                    OverlayItem {
                        text: "2. Song C".to_string(),
                        selected: false,
                    },
                ],
                help_lines: vec!["1-5 choose   Enter play   Q stop".to_string()],
            }),
        );

        let lines = compose_lines(&view, 80);
        assert!(lines.iter().any(|line| line.text == "Up Next"));
        assert!(
            lines
                .iter()
                .any(|line| line.text.contains("Autoplaying 1 in 8s"))
        );
        assert!(lines.iter().any(|line| {
            line.text.contains("1. Song B") && matches!(line.style, LineStyle::Selected)
        }));
    }
}
