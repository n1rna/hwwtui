//! Ratatui rendering for hwwtui.
//!
//! Layout:
//!
//! ```text
//! ┌─ hwwtui ──────────────────────────────────────────────────┐
//! │ [Trezor ▶] [BitBox02 ■] [Coldcard ■] ...               │ <- device tab bar
//! ├─────────────────────────────┬─────────────────────────────┤
//! │ LEFT PANEL                  │ RIGHT PANEL                 │
//! │ [Controls] [Screen] [Keys]  │ [Methods] [Firmware] [Raw]  │
//! │                             │ [Bridge]                    │
//! │  (content of active tab)    │  (content of active tab)   │
//! ├─────────────────────────────┴─────────────────────────────┤
//! │ Status bar                                                │
//! └───────────────────────────────────────────────────────────┘
//! ```

use std::sync::Mutex;

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Tabs},
    Frame,
};

use bundler::BundleStatus;

use crate::app::{format_bytes, Action, App};
use emulators::EmulatorStatus;

// ── Colours ───────────────────────────────────────────────────────────────────

const COLOR_RUNNING: Color = Color::Green;
const COLOR_STOPPED: Color = Color::DarkGray;
const COLOR_STARTING: Color = Color::Yellow;
const COLOR_ERROR: Color = Color::Red;
const COLOR_ACTIVE_TAB: Color = Color::Cyan;
const COLOR_PANEL_TAB: Color = Color::Blue;
const COLOR_HOST_TO_DEV: Color = Color::Yellow;
const COLOR_DEV_TO_HOST: Color = Color::Cyan;
const COLOR_BUNDLE_OK: Color = Color::Green;
const COLOR_BUNDLE_MISSING: Color = Color::DarkGray;
const COLOR_BUNDLE_PROGRESS: Color = Color::Yellow;
const COLOR_BUNDLE_FAIL: Color = Color::Red;

// ── Click-zone registry ───────────────────────────────────────────────────────

/// A recorded clickable region with its associated action.
#[derive(Clone)]
struct ClickZone {
    x: u16,
    y: u16,
    width: u16,
    action: Action,
}

/// Global click zone registry updated every render frame.
static CLICK_ZONES: Mutex<Vec<ClickZone>> = Mutex::new(Vec::new());

fn register_zone(x: u16, y: u16, width: u16, action: Action) {
    if let Ok(mut zones) = CLICK_ZONES.lock() {
        zones.push(ClickZone { x, y, width, action });
    }
}

/// Called by the event loop when a left-click occurs.
pub fn handle_mouse_click(app: &mut App, col: u16, row: u16) {
    if let Ok(zones) = CLICK_ZONES.lock() {
        for zone in zones.iter() {
            if row == zone.y && col >= zone.x && col < zone.x + zone.width {
                app.dispatch(zone.action.clone());
                return;
            }
        }
    }
}

// ── Main render ───────────────────────────────────────────────────────────────

pub fn render(frame: &mut Frame, app: &App) {
    // Clear click zones from the previous frame.
    if let Ok(mut zones) = CLICK_ZONES.lock() {
        zones.clear();
    }

    let area = frame.area();

    // Vertical split: device tab bar / body / status bar.
    let [tab_area, body_area, status_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);

    render_device_tabs(frame, app, tab_area);
    render_body(frame, app, body_area);
    render_status_bar(frame, app, status_area);
}

// ── Device tab bar ────────────────────────────────────────────────────────────

fn render_device_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = app
        .panes
        .iter()
        .enumerate()
        .map(|(i, pane)| {
            let indicator = status_indicator(pane);
            let is_selected = i == app.selected_tab;
            let color = if is_selected {
                COLOR_ACTIVE_TAB
            } else {
                Color::White
            };
            Line::from(vec![
                Span::styled(pane.label.clone(), Style::default().fg(color)),
                Span::raw(" "),
                Span::styled(indicator, Style::default().fg(color)),
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

    // Register click zones for each device tab.
    // Approximate tab positions: starts after the block border and title.
    // The title " hwwtui " is 9 chars + 1 border = inner starts at x=1.
    // Each tab is separated by "  " (2 chars); we approximate by scanning.
    let inner_x = area.x + 1; // left border
    let tab_y = area.y + 1; // middle of the 3-row tab bar
    let title_offset = " hwwtui ".len() as u16 + 1; // skip block title
    let mut cursor_x = inner_x + title_offset;
    for (i, pane) in app.panes.iter().enumerate() {
        // Tab text width: label + " " + indicator (1 char).
        let tab_width = (pane.label.len() + 2) as u16;
        register_zone(cursor_x, tab_y, tab_width, Action::SelectTab(i));
        cursor_x += tab_width + 2; // +2 for the "  " divider
    }
}

fn status_indicator(pane: &crate::app::DevicePane) -> String {
    match pane.emulator.as_ref().map(|e| e.status()) {
        Some(EmulatorStatus::Running) => "▶".to_string(),
        Some(EmulatorStatus::Starting) => "◑".to_string(),
        Some(EmulatorStatus::Error(_)) => "✗".to_string(),
        Some(EmulatorStatus::Stopped) | None => match &pane.bundle_status {
            BundleStatus::Installed { .. } => "■".to_string(),
            BundleStatus::Downloading { .. } => "↓".to_string(),
            BundleStatus::NotInstalled => "○".to_string(),
            BundleStatus::Failed { .. } => "✗".to_string(),
        },
    }
}

// ── Body ──────────────────────────────────────────────────────────────────────

fn render_body(frame: &mut Frame, app: &App, area: Rect) {
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .areas(area);

    render_left_panel(frame, app, left_area);
    render_right_panel(frame, app, right_area);
}

// ── Left panel ────────────────────────────────────────────────────────────────

const LEFT_TABS: &[&str] = &["Controls", "Screen", "Keys"];

fn render_left_panel(frame: &mut Frame, app: &App, area: Rect) {
    // Split into sub-tab bar + content.
    let [tab_bar_area, content_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);

    render_panel_tabs(frame, app.left_tab, LEFT_TABS, tab_bar_area, true, 0);

    let pane = app.selected_pane();
    match app.left_tab {
        0 => render_controls_tab(frame, pane, app, content_area),
        1 => render_screen_tab(frame, pane, content_area),
        2 => render_keys_tab(frame, pane, content_area),
        _ => {}
    }
}

// ── Right panel ───────────────────────────────────────────────────────────────

const RIGHT_TABS: &[&str] = &["Methods", "Firmware", "Raw", "Bridge"];

fn render_right_panel(frame: &mut Frame, app: &App, area: Rect) {
    let [tab_bar_area, content_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);

    render_panel_tabs(frame, app.right_tab, RIGHT_TABS, tab_bar_area, false, 4);

    let pane = app.selected_pane();
    match app.right_tab {
        0 => render_methods_tab(frame, pane, content_area),
        1 => render_firmware_tab(frame, pane, content_area),
        2 => render_raw_tab(frame, pane, content_area),
        3 => render_bridge_tab(frame, pane, content_area),
        _ => {}
    }
}

// ── Panel tab bar ─────────────────────────────────────────────────────────────

/// Render a horizontal sub-tab bar for a panel.
///
/// `is_left` controls whether the action produced is `SetLeftTab` or `SetRightTab`.
/// `key_offset` is the key number to show (1 for left, 5 for right).
fn render_panel_tabs(
    frame: &mut Frame,
    selected: usize,
    labels: &[&str],
    area: Rect,
    is_left: bool,
    key_offset: usize,
) {
    let mut spans: Vec<Span> = Vec::new();
    let mut cursor_x = area.x;

    for (i, &label) in labels.iter().enumerate() {
        let key_num = key_offset + i + 1;
        let is_active = i == selected;

        let prefix = format!("[{key_num}] ");
        let tab_text = format!("{prefix}{label}  ");
        let tab_width = tab_text.len() as u16;

        let style = if is_active {
            Style::default()
                .fg(Color::Black)
                .bg(COLOR_PANEL_TAB)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLOR_PANEL_TAB)
        };

        spans.push(Span::styled(tab_text, style));

        // Register click zone.
        let action = if is_left {
            Action::SetLeftTab(i)
        } else {
            Action::SetRightTab(i)
        };
        register_zone(cursor_x, area.y, tab_width, action);
        cursor_x += tab_width;
    }

    let line = Line::from(spans);
    let para = Paragraph::new(line).alignment(Alignment::Left);
    frame.render_widget(para, area);
}

// ── Left tab: Controls ────────────────────────────────────────────────────────

fn render_controls_tab(
    frame: &mut Frame,
    pane: &crate::app::DevicePane,
    app: &App,
    area: Rect,
) {
    let is_downloading = matches!(pane.bundle_status, BundleStatus::Downloading { .. });

    let (content_area, progress_area) = if is_downloading {
        let [c, p] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(3)]).areas(area);
        (c, Some(p))
    } else {
        (area, None)
    };

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

    let _ = app; // reserved for future use

    let lines = vec![
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
                pane.status_str(),
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
            Span::raw("  Bridge:     "),
            Span::styled(&bridge_str, Style::default().fg(Color::Magenta)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Device actions",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )),
        Line::from(vec![
            Span::styled(
                "  [s]",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Start   "),
            Span::styled(
                "[x]",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Stop   "),
            Span::styled(
                "[r]",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Reset"),
        ]),
        Line::from(vec![
            Span::styled(
                "  [d]",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Download   "),
            Span::styled(
                "[D]",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Remove bundle"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Navigation",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )),
        Line::from(vec![
            Span::styled("  [Tab]", Style::default().fg(Color::Cyan)),
            Span::raw(" Next device   "),
            Span::styled("[Shift+Tab]", Style::default().fg(Color::Cyan)),
            Span::raw(" Prev device"),
        ]),
        Line::from(vec![
            Span::styled("  [1-3]", Style::default().fg(Color::Blue)),
            Span::raw(" Left panel   "),
            Span::styled("[5-8]", Style::default().fg(Color::Blue)),
            Span::raw(" Right panel"),
        ]),
        Line::from(vec![
            Span::styled("  [q]", Style::default().fg(Color::White)),
            Span::raw(" Quit"),
        ]),
    ];

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Controls ")
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(para, content_area);

    if let Some(p) = progress_area {
        render_download_progress(frame, pane, p);
    }
}

// ── Left tab: Screen ──────────────────────────────────────────────────────────

fn render_screen_tab(frame: &mut Frame, pane: &crate::app::DevicePane, area: Rect) {
    let title = if pane.debug_link.is_some() {
        format!(" {} — Debug Screen ", pane.label)
    } else {
        format!(" {} — Emulator Output ", pane.label)
    };

    let mut lines: Vec<Line> = Vec::new();

    if pane.is_running() {
        let has_content = !pane.screen_title.is_empty()
            || !pane.screen_content.is_empty()
            || !pane.screen_buttons.is_empty();

        if has_content {
            if !pane.screen_title.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("  {}", pane.screen_title),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
            }

            for line in &pane.screen_content {
                lines.push(Line::from(Span::styled(
                    format!("  {line}"),
                    Style::default().fg(Color::White),
                )));
            }

            if !pane.screen_buttons.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(
                    pane.screen_buttons
                        .iter()
                        .map(|b| {
                            Span::styled(
                                format!(" [{b}] "),
                                Style::default()
                                    .fg(Color::Cyan)
                                    .add_modifier(Modifier::BOLD),
                            )
                        })
                        .collect::<Vec<_>>(),
                ));
            }
        } else if pane.debug_link.is_some() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  Waiting for screen layout…",
                Style::default().fg(Color::DarkGray),
            )));
        } else if !pane.firmware_log.is_empty() {
            let max_lines = area.height.saturating_sub(2) as usize;
            let skip = pane.firmware_log.len().saturating_sub(max_lines);
            for line in pane.firmware_log.iter().skip(skip) {
                lines.push(Line::from(Span::styled(
                    format!("  {line}"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        } else {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  Emulator running — no output yet",
                Style::default().fg(Color::DarkGray),
            )));
        }
    } else {
        match &pane.bundle_status {
            BundleStatus::NotInstalled => {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  Press [d] to download the firmware bundle",
                    Style::default().fg(Color::DarkGray),
                )));
            }
            BundleStatus::Downloading { progress_pct } => {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("  Downloading bundle… {progress_pct}%"),
                    Style::default().fg(COLOR_BUNDLE_PROGRESS),
                )));
            }
            BundleStatus::Installed { .. } => {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  Press [s] to start the emulator",
                    Style::default().fg(Color::DarkGray),
                )));
            }
            BundleStatus::Failed { error } => {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("  Download failed: {error}"),
                    Style::default().fg(COLOR_ERROR),
                )));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  Press [d] to retry",
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
    }

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(para, area);
}

// ── Left tab: Keys ────────────────────────────────────────────────────────────

fn render_keys_tab(frame: &mut Frame, pane: &crate::app::DevicePane, area: Rect) {
    use crate::config::DeviceKind;

    let mut lines = vec![Line::from("")];

    match pane.kind {
        DeviceKind::Trezor => {
            lines.push(Line::from(Span::styled(
                "  Trezor Debug Link",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "  [Enter]",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  Confirm / press YES"),
            ]));
            lines.push(Line::from(vec![
                Span::styled(
                    "  [Esc]  ",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  Cancel / press NO"),
            ]));
            lines.push(Line::from(vec![
                Span::styled(
                    "  [↑↓←→]",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  Swipe gesture"),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "  [i]",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("     Send Initialize"),
            ]));
            lines.push(Line::from(vec![
                Span::styled(
                    "  [l]",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("     Load test seed"),
            ]));
        }
        DeviceKind::BitBox02 => {
            lines.push(Line::from(Span::styled(
                "  BitBox02 Simulator",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "  [l]",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  Initialize with test mnemonic"),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  UHID bridge not yet active for this device.",
                Style::default().fg(Color::DarkGray),
            )));
        }
        _ => {
            lines.push(Line::from(Span::styled(
                format!("  {} Simulator", pane.label),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  Device-specific key bindings not yet configured.",
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  UHID bridge work required for desktop wallet integration.",
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Key Bindings ")
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(para, area);
}

// ── Right tab: Methods ────────────────────────────────────────────────────────

fn render_methods_tab(frame: &mut Frame, pane: &crate::app::DevicePane, area: Rect) {
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
            .title(" Method Calls ")
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(list, area);
}

// ── Right tab: Firmware ───────────────────────────────────────────────────────

fn render_firmware_tab(frame: &mut Frame, pane: &crate::app::DevicePane, area: Rect) {
    let max_items = area.height.saturating_sub(2) as usize;
    let items: Vec<ListItem> = pane
        .firmware_log
        .iter()
        .rev()
        .take(max_items)
        .rev()
        .map(|line| {
            let max_width = area.width.saturating_sub(4) as usize;
            let display = if line.len() > max_width && max_width > 1 {
                format!("{}…", &line[..max_width.saturating_sub(1)])
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
            .title(" Firmware Log ")
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(list, area);
}

// ── Right tab: Raw ────────────────────────────────────────────────────────────

fn render_raw_tab(frame: &mut Frame, pane: &crate::app::DevicePane, area: Rect) {
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
            .title(" Raw Messages ")
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(list, area);
}

// ── Right tab: Bridge ─────────────────────────────────────────────────────────

fn render_bridge_tab(frame: &mut Frame, pane: &crate::app::DevicePane, area: Rect) {
    let bridge_running = pane
        .bridge
        .as_ref()
        .map(|b| b.is_running())
        .unwrap_or(false);

    let (bridge_status, bridge_color) = if bridge_running {
        ("Active ●", COLOR_RUNNING)
    } else {
        ("Inactive ○", COLOR_STOPPED)
    };

    let connection_str = if bridge_running {
        format!("/dev/uhid → {}", pane.transport_label)
    } else {
        "Not connected".to_string()
    };

    let method_count = pane.method_log.len();
    let raw_count = pane.raw_log.len();

    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  Status:      "),
            Span::styled(
                bridge_status,
                Style::default()
                    .fg(bridge_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::raw("  Connection:  "),
            Span::styled(&connection_str, Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  Messages intercepted:"),
        ]),
        Line::from(vec![
            Span::raw("    Decoded:  "),
            Span::styled(
                format!("{method_count}"),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::raw("    Raw:      "),
            Span::styled(
                format!("{raw_count}"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  UHID bridge relays HID packets between the",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  hardware wallet emulator and desktop wallets.",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Bridge Status ")
            .border_style(Style::default().fg(Color::DarkGray)),
    );
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

// ── Status bar ────────────────────────────────────────────────────────────────

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let pane = app.selected_pane();

    let status_color = match pane.emulator.as_ref().map(|e| e.status()) {
        Some(EmulatorStatus::Running) => COLOR_RUNNING,
        Some(EmulatorStatus::Starting) => COLOR_STARTING,
        Some(EmulatorStatus::Error(_)) => COLOR_ERROR,
        _ => COLOR_STOPPED,
    };

    let status_indicator = match pane.emulator.as_ref().map(|e| e.status()) {
        Some(EmulatorStatus::Running) => "●",
        Some(EmulatorStatus::Starting) => "◑",
        Some(EmulatorStatus::Error(_)) => "✗",
        _ => "○",
    };

    let bridge_indicator = if pane
        .bridge
        .as_ref()
        .map(|b| b.is_running())
        .unwrap_or(false)
    {
        Span::styled("UHID ●", Style::default().fg(COLOR_RUNNING))
    } else {
        Span::styled("UHID ○", Style::default().fg(COLOR_STOPPED))
    };

    let line = Line::from(vec![
        Span::raw(" Status: "),
        Span::styled(
            format!("{} {status_indicator}", pane.status_str()),
            Style::default().fg(status_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  |  Transport: "),
        Span::styled(&pane.transport_label, Style::default().fg(Color::White)),
        Span::raw("  |  Bridge: "),
        bridge_indicator,
        Span::styled(
            "  |  [s]tart [x]stop [r]eset [d]ownload [Tab]next [q]uit",
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    let para = Paragraph::new(line)
        .style(Style::default().bg(Color::Black))
        .alignment(Alignment::Left);
    frame.render_widget(para, area);
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
