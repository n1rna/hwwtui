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
//! | `q` / `Ctrl-C` | Quit |

mod app;
mod config;
mod ui;

use anyhow::Context;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
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

    let cfg = config::Config::from_env_or_defaults();

    // Set up the terminal.
    enable_raw_mode().context("Failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("Failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Failed to create terminal")?;

    let mut app = App::new(cfg).context("Failed to initialise application")?;

    let result = run_event_loop(&mut terminal, &mut app).await;

    // Always restore the terminal, even on error.
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
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

        // Update download progress from background tasks.
        app.poll_download_progress();

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
                        _ => {}
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
