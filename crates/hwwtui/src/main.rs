//! hwwtui — hardware wallet emulator manager and UHID bridge TUI.
//!
//! # Quick start
//!
//! ```text
//! # Build the Trezor emulator first:
//! #   cd ~/trezor-firmware/core && make build_unix
//!
//! # Run hwwtui (needs /dev/uhid access):
//! sudo hwwtui --trezor-firmware /path/to/trezor-firmware/core
//! ```
//!
//! # Keybindings
//!
//! | Key | Action |
//! |-----|--------|
//! | `Tab` / `Shift+Tab` | Cycle device tabs |
//! | `s` | Start selected device |
//! | `x` | Stop selected device |
//! | `r` | Reset (stop + start) |
//! | `d` | Download firmware bundle for selected device |
//! | `D` | Remove installed bundle for selected device |
//! | `Enter` | Confirm (press YES via debug link) |
//! | `Esc` | Cancel (press NO via debug link) |
//! | `↑` / `↓` / `←` / `→` | Swipe gesture via debug link |
//! | `q` / `Ctrl-C` | Quit |

mod app;
mod config;
mod ui;

use anyhow::Context;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{io, time::Duration};
use tracing::info;

use app::{Action, App};

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Set up tracing to a file so it doesn't interfere with the TUI.
    init_tracing();

    info!("hwwtui starting");

    check_uhid_access();

    let cfg = config::Config::from_env_or_defaults();

    // Set up the terminal.
    enable_raw_mode().context("Failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )
    .context("Failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Failed to create terminal")?;

    let mut app = App::new(cfg).context("Failed to initialise application")?;

    let result = run_event_loop(&mut terminal, &mut app).await;

    // Always restore the terminal, even on error.
    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )
    .ok();
    terminal.show_cursor().ok();

    if let Err(e) = result {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }

    info!("hwwtui exiting cleanly");
    Ok(())
}

// ── Event loop ────────────────────────────────────────────────────────────────

async fn run_event_loop<B>(terminal: &mut Terminal<B>, app: &mut App) -> anyhow::Result<()>
where
    B: ratatui::backend::Backend,
    B::Error: Send + Sync + 'static,
{
    const TICK: Duration = Duration::from_millis(100);

    loop {
        // Draw the current frame.
        terminal.draw(|frame| ui::render(frame, app))?;

        // Drain any pending bridge messages (non-blocking).
        app.poll_bridge_messages();

        // Drain captured emulator process output (non-blocking).
        app.poll_firmware_logs();

        // Update download progress from background tasks.
        app.poll_download_progress();

        // Poll the debug-link screen (throttled internally to ~500 ms).
        app.poll_screen().await;

        // Check if any emulators need a health refresh.
        // We do it inline here; a real impl would use a periodic background task.

        // Poll for input events with a short timeout so the TUI stays responsive.
        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(key) => {
                    // Ctrl-C / q → quit.
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c')
                    {
                        app.stop_all().await;
                        return Ok(());
                    }

                    match key.code {
                        KeyCode::Char('q') => {
                            app.stop_all().await;
                            return Ok(());
                        }
                        KeyCode::Tab => app.dispatch(Action::NextTab),
                        KeyCode::BackTab => app.dispatch(Action::PrevTab),
                        KeyCode::Char('s') => app.dispatch(Action::StartSelected),
                        KeyCode::Char('x') => app.dispatch(Action::StopSelected),
                        KeyCode::Char('r') => app.dispatch(Action::ResetSelected),
                        KeyCode::Char('d') => app.dispatch(Action::DownloadSelected),
                        // Shift+D (uppercase D) → remove bundle.
                        KeyCode::Char('D') => app.dispatch(Action::RemoveSelected),
                        // Wire protocol commands.
                        KeyCode::Char('i') => app.dispatch(Action::InitializeDevice),
                        KeyCode::Char('l') => {
                            use config::DeviceKind;
                            match app.selected_pane().kind {
                                DeviceKind::Trezor => app.dispatch(Action::LoadTestSeed),
                                DeviceKind::BitBox02 => app.dispatch(Action::InitializeBitBox02),
                                _ => {}
                            }
                        }
                        // Left panel tab selection (1–3).
                        KeyCode::Char('1') => app.dispatch(Action::SetLeftTab(0)),
                        KeyCode::Char('2') => app.dispatch(Action::SetLeftTab(1)),
                        KeyCode::Char('3') => app.dispatch(Action::SetLeftTab(2)),
                        // Right panel tab selection (5–8).
                        KeyCode::Char('5') => app.dispatch(Action::SetRightTab(0)),
                        KeyCode::Char('6') => app.dispatch(Action::SetRightTab(1)),
                        KeyCode::Char('7') => app.dispatch(Action::SetRightTab(2)),
                        KeyCode::Char('8') => app.dispatch(Action::SetRightTab(3)),
                        // Debug link: confirm / cancel / swipe.
                        KeyCode::Enter => app.dispatch(Action::ConfirmSelected),
                        KeyCode::Esc => app.dispatch(Action::CancelSelected),
                        KeyCode::Up => app.dispatch(Action::SwipeUp),
                        KeyCode::Down => app.dispatch(Action::SwipeDown),
                        KeyCode::Left => app.dispatch(Action::SwipeLeft),
                        KeyCode::Right => app.dispatch(Action::SwipeRight),
                        _ => {}
                    }
                }
                Event::Mouse(mouse) => {
                    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                        ui::handle_mouse_click(app, mouse.column, mouse.row);
                    }
                }
                Event::Resize(_, _) => {} // terminal handles redraw automatically
                _ => {}
            }
        }

        // Handle queued actions (start/stop are async; we drive them here).
        app.process_actions().await;

        if app.should_quit() {
            break;
        }
    }

    Ok(())
}

// ── Tracing setup ─────────────────────────────────────────────────────────────

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    // Log to a file in /tmp so the TUI screen is not polluted.
    let log_path = std::env::temp_dir().join("hwwtui.log");
    let log_file = std::fs::File::options()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("Cannot open log file");

    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env().add_directive(tracing::Level::DEBUG.into()))
        .with(fmt::layer().with_writer(log_file).with_ansi(false))
        .init();

    // Print the log path so the user can tail it in another terminal.
    eprintln!("Logging to {}", log_path.display());
}

// ── Permission checks ────────────────────────────────────────────────────────

/// Check /dev/uhid access and print a helpful message if missing.
/// Does NOT abort — the TUI will still start, and the bridge will log
/// a non-fatal warning when it fails to create a UHID device.
fn check_uhid_access() {
    use std::path::Path;

    let uhid = Path::new("/dev/uhid");
    if !uhid.exists() {
        eprintln!(
            "Warning: /dev/uhid not found. UHID bridge will be unavailable.\n\
             Load the kernel module: sudo modprobe uhid"
        );
        return;
    }

    if std::fs::OpenOptions::new().write(true).open(uhid).is_err() {
        eprintln!(
            "Warning: /dev/uhid is not writable. UHID bridge will be unavailable.\n\
             Run:  just setup-udev\n\
             Or:   sudo cp udev/99-hwwtui.rules /etc/udev/rules.d/ && \
sudo udevadm control --reload-rules && sudo udevadm trigger"
        );
    }
}
