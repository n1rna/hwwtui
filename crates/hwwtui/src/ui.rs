//! Ratatui rendering for hwwtui.
//!
//! Layout (per device tab):
//!
//! ```text
//! ┌─ hwwtui ──────────────────────────────────────────────────┐
//! │ [Trezor ▶] [BitBox02 ■] [Coldcard ■] ...               │ <- tab bar
//! ├──────────────────────────┬────────────────────────────────┤
//! │  Device Screen           │  Method Calls                  │
//! │  (placeholder)           │  → Initialize                  │
//! │                          │  ← Features                   │
//! ├──────────────────────────┤────────────────────────────────┤
//! │  Controls                │  Firmware Log                  │
//! │  Status: Running ●       │  trezor.loop DEBUG spawn...    │
//! │  Transport: UDP :21324   │  trezor.workflow DEBUG start.. │
//! ├──────────────────────────┴────────────────────────────────┤
//! │  Raw Messages                                             │
//! │  >> 3f 23 23 00 00 ...  << 3f 23 23 00 11 ...            │
//! └───────────────────────────────────────────────────────────┘
//! ```

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Tabs},
    Frame,
};

use bundler::BundleStatus;

use crate::app::{format_bytes, App};
use emulators::EmulatorStatus;

// ── Colours ───────────────────────────────────────────────────────────────────

const COLOR_RUNNING: Color = Color::Green;
const COLOR_STOPPED: Color = Color::DarkGray;
const COLOR_STARTING: Color = Color::Yellow;
const COLOR_ERROR: Color = Color::Red;
const COLOR_ACTIVE_TAB: Color = Color::Cyan;
const COLOR_HOST_TO_DEV: Color = Color::Yellow;
const COLOR_DEV_TO_HOST: Color = Color::Cyan;
const COLOR_BUNDLE_OK: Color = Color::Green;
const COLOR_BUNDLE_MISSING: Color = Color::DarkGray;
const COLOR_BUNDLE_PROGRESS: Color = Color::Yellow;
const COLOR_BUNDLE_FAIL: Color = Color::Red;

// ── Main render ───────────────────────────────────────────────────────────────

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Outer vertical split: tab bar / body.
    let [tab_area, body_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).areas(area);

    render_tabs(frame, app, tab_area);
    render_body(frame, app, body_area);
}

// ── Tab bar ───────────────────────────────────────────────────────────────────

fn render_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = app
        .panes
        .iter()
        .map(|pane| {
            let indicator = status_indicator(pane);
            let style = Style::default().fg(COLOR_ACTIVE_TAB);
            Line::from(vec![
                Span::styled(pane.label.clone(), style),
                Span::raw(" "),
                Span::styled(indicator, style),
            ])
        })
        .collect();

    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title(" hwwtui "))
        .select(app.selected_tab)
        .style(Style::default().fg(Color::White))
        .highlight_style(
            Style::default()
                .fg(COLOR_ACTIVE_TAB)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )
        .divider(Span::raw("  "));

    frame.render_widget(tabs, area);
}

fn status_indicator(pane: &crate::app::DevicePane) -> String {
    match pane.emulator.as_ref().map(|e| e.status()) {
        Some(EmulatorStatus::Running) => "▶".to_string(),
        Some(EmulatorStatus::Starting) => "◑".to_string(),
        Some(EmulatorStatus::Error(_)) => "✗".to_string(),
        Some(EmulatorStatus::Stopped) | None => {
            // Show bundle status in the tab indicator when emulator is stopped.
            match &pane.bundle_status {
                BundleStatus::Installed { .. } => "■".to_string(),
                BundleStatus::Downloading { .. } => "↓".to_string(),
                BundleStatus::NotInstalled => "○".to_string(),
                BundleStatus::Failed { .. } => "✗".to_string(),
            }
        }
    }
}

// ── Body ──────────────────────────────────────────────────────────────────────

fn render_body(frame: &mut Frame, app: &App, area: Rect) {
    // Outer vertical split: upper columns / raw messages strip at the bottom.
    let [upper_area, raw_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(6)]).areas(area);

    // Upper area: left column (screen + controls) / right column (method log + firmware log).
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .areas(upper_area);

    let pane = app.selected_pane();

    // Left column: screen mirror (fill) + optional progress bar + controls (fixed 9 rows).
    let is_downloading = matches!(pane.bundle_status, BundleStatus::Downloading { .. });

    let (screen_area, controls_area, progress_area) = if is_downloading {
        let [s, p, c] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(3),
            Constraint::Length(9),
        ])
        .areas(left_area);
        (s, c, Some(p))
    } else {
        let [s, c] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(9)]).areas(left_area);
        (s, c, None)
    };

    // Right column: method log (fill) + firmware log (fixed 10 rows).
    let [method_area, firmware_log_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(10)]).areas(right_area);

    render_screen_mirror(frame, pane, screen_area);
    render_controls(frame, pane, controls_area);
    if let Some(p) = progress_area {
        render_download_progress(frame, pane, p);
    }
    render_method_log(frame, pane, method_area);
    render_firmware_log(frame, pane, firmware_log_area);
    render_raw_log(frame, pane, raw_area);
}

// ── Screen mirror ─────────────────────────────────────────────────────────────

fn render_screen_mirror(frame: &mut Frame, pane: &crate::app::DevicePane, area: Rect) {
    let title = format!(" {} — Screen ", pane.label);
    let inner_style = if pane.is_running() {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let content = if pane.is_running() {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                "  (Screen mirror not yet implemented)",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Trezor emulator screen is available on UDP :21325",
                Style::default().fg(Color::DarkGray),
            )),
        ]
    } else {
        match &pane.bundle_status {
            BundleStatus::NotInstalled => vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Press [d] to download the firmware bundle",
                    Style::default().fg(Color::DarkGray),
                )),
            ],
            BundleStatus::Downloading { progress_pct } => vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("  Downloading bundle… {progress_pct}%"),
                    Style::default().fg(COLOR_BUNDLE_PROGRESS),
                )),
            ],
            BundleStatus::Installed { .. } => vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Press [s] to start the emulator",
                    Style::default().fg(Color::DarkGray),
                )),
            ],
            BundleStatus::Failed { error } => vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("  Download failed: {error}"),
                    Style::default().fg(COLOR_ERROR),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  Press [d] to retry",
                    Style::default().fg(Color::DarkGray),
                )),
            ],
        }
    };

    let para = Paragraph::new(content)
        .block(Block::default().borders(Borders::ALL).title(title))
        .style(inner_style);
    frame.render_widget(para, area);
}

// ── Controls ──────────────────────────────────────────────────────────────────

fn render_controls(frame: &mut Frame, pane: &crate::app::DevicePane, area: Rect) {
    let status_str = pane.status_str();
    let status_color = match pane.emulator.as_ref().map(|e| e.status()) {
        Some(EmulatorStatus::Running) => COLOR_RUNNING,
        Some(EmulatorStatus::Starting) => COLOR_STARTING,
        Some(EmulatorStatus::Error(_)) => COLOR_ERROR,
        _ => COLOR_STOPPED,
    };

    let bridge_str = if pane
        .bridge
        .as_ref()
        .map(|b| b.is_running())
        .unwrap_or(false)
    {
        format!("/dev/uhid → {}", pane.transport_label)
    } else {
        "—".to_string()
    };

    let (bundle_str, bundle_color) = bundle_status_display(&pane.bundle_status);

    let lines = vec![
        Line::from(vec![
            Span::styled(
                "[s] Start  ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "[x] Stop  ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "[r] Reset  ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("[Tab] Next  ", Style::default().fg(Color::Cyan)),
            Span::styled("[q] Quit", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled(
                "[d] Download  ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "[D] Remove",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  Bundle:     "),
            Span::styled(
                &bundle_str,
                Style::default()
                    .fg(bundle_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::raw("  Status:     "),
            Span::styled(
                &status_str,
                Style::default()
                    .fg(status_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::raw("  Transport:  "),
            Span::styled(&pane.transport_label, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::raw("  UHID:       "),
            Span::styled(&bridge_str, Style::default().fg(Color::Magenta)),
        ]),
    ];

    let para =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Controls "));
    frame.render_widget(para, area);
}

// ── Download progress bar ─────────────────────────────────────────────────────

fn render_download_progress(frame: &mut Frame, pane: &crate::app::DevicePane, area: Rect) {
    let (pct, label) = match &pane.bundle_status {
        BundleStatus::Downloading { progress_pct } => (
            *progress_pct,
            format!(" Downloading {}… {}% ", pane.label, progress_pct),
        ),
        _ => (0, " Download complete ".to_string()),
    };

    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title(label))
        .gauge_style(
            Style::default()
                .fg(COLOR_BUNDLE_PROGRESS)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .ratio(pct as f64 / 100.0);

    frame.render_widget(gauge, area);
}

// ── Method log ────────────────────────────────────────────────────────────────

fn render_method_log(frame: &mut Frame, pane: &crate::app::DevicePane, area: Rect) {
    let max_items = area.height.saturating_sub(2) as usize;
    let items: Vec<ListItem> = pane
        .method_log
        .iter()
        .rev()
        .take(max_items)
        .rev()
        .map(|(dir, text)| {
            let (prefix, color) = direction_style(dir);
            ListItem::new(Line::from(vec![
                Span::styled(
                    prefix,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {text}"), Style::default().fg(Color::White)),
            ]))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Method Calls "),
    );
    frame.render_widget(list, area);
}

// ── Firmware log ──────────────────────────────────────────────────────────────

fn render_firmware_log(frame: &mut Frame, pane: &crate::app::DevicePane, area: Rect) {
    let max_items = area.height.saturating_sub(2) as usize;
    let items: Vec<ListItem> = pane
        .firmware_log
        .iter()
        .rev()
        .take(max_items)
        .rev()
        .map(|line| {
            // Truncate lines that would overflow the widget width.
            let max_width = area.width.saturating_sub(4) as usize;
            let display = if line.len() > max_width && max_width > 1 {
                format!("{}…", &line[..max_width - 1])
            } else {
                line.clone()
            };
            ListItem::new(Line::from(Span::styled(
                display,
                Style::default().fg(Color::DarkGray),
            )))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Firmware Log "),
    );
    frame.render_widget(list, area);
}

// ── Raw log ───────────────────────────────────────────────────────────────────

fn render_raw_log(frame: &mut Frame, pane: &crate::app::DevicePane, area: Rect) {
    let max_items = area.height.saturating_sub(2) as usize;
    let items: Vec<ListItem> = pane
        .raw_log
        .iter()
        .rev()
        .take(max_items)
        .rev()
        .map(|(dir, hex)| {
            let (prefix, color) = direction_style(dir);
            ListItem::new(Line::from(vec![
                Span::styled(
                    prefix,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {hex}"), Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Raw Messages "),
    );
    frame.render_widget(list, area);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns the display string and colour for a `BundleStatus`.
fn bundle_status_display(status: &BundleStatus) -> (String, Color) {
    match status {
        BundleStatus::NotInstalled => ("Not installed".to_string(), COLOR_BUNDLE_MISSING),
        BundleStatus::Downloading { progress_pct } => (
            format!("Downloading {progress_pct}%"),
            COLOR_BUNDLE_PROGRESS,
        ),
        BundleStatus::Installed {
            version,
            size_bytes,
        } => (
            format!("{version} ({})", format_bytes(*size_bytes)),
            COLOR_BUNDLE_OK,
        ),
        BundleStatus::Failed { error } => (format!("Failed: {error}"), COLOR_BUNDLE_FAIL),
    }
}

/// Returns a display prefix and colour for a direction string.
fn direction_style(dir: &str) -> (&'static str, Color) {
    match dir {
        ">>" => (">>", COLOR_HOST_TO_DEV),
        "<<" => ("<<", COLOR_DEV_TO_HOST),
        "→" => (" →", COLOR_HOST_TO_DEV),
        "←" => (" ←", COLOR_DEV_TO_HOST),
        "■" => (" ■", COLOR_STOPPED),
        "!" => (" !", COLOR_ERROR),
        _ => (" •", Color::White),
    }
}
