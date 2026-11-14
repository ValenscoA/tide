mod api;
mod app;
mod audio;
mod auth;
mod config;
mod cover;
mod download_queue;
mod lyrics;
mod metadata;
mod mpris;
mod spectrum;
mod theme;
mod ui;
mod utils;

use std::io;
#[cfg(unix)]
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use crate::app::App;

#[derive(Parser, Debug)]
#[command(
    name = "tide",
    version,
    about = "Tide — terminal music player for Tidal"
)]
struct Cli {
    /// Start directly with a Tidal search query
    #[arg(short, long)]
    search: Option<String>,

    /// Start in Local Files browser
    #[arg(short, long)]
    local: bool,
}

#[cfg(unix)]
static SAVED_STDERR: AtomicI32 = AtomicI32::new(-1);

#[cfg(unix)]
fn silence_stderr() {
    use std::fs::OpenOptions;
    use std::os::unix::io::AsRawFd;

    extern "C" {
        fn dup(oldfd: i32) -> i32;
        fn dup2(oldfd: i32, newfd: i32) -> i32;
        fn close(fd: i32) -> i32;
    }

    if SAVED_STDERR.load(Ordering::Relaxed) >= 0 {
        return;
    }
    if let Ok(devnull) = OpenOptions::new().write(true).open("/dev/null") {
        unsafe {
            let saved = dup(2);
            if saved >= 0 {
                if SAVED_STDERR
                    .compare_exchange(-1, saved, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    let _ = dup2(devnull.as_raw_fd(), 2);
                } else {
                    let _ = close(saved);
                }
            }
        }
    }
}

#[cfg(unix)]
fn restore_stderr() {
    extern "C" {
        fn dup2(oldfd: i32, newfd: i32) -> i32;
        fn close(fd: i32) -> i32;
    }

    let saved = SAVED_STDERR.swap(-1, Ordering::SeqCst);
    if saved >= 0 {
        unsafe {
            let _ = dup2(saved, 2);
            let _ = close(saved);
        }
    }
}

#[cfg(not(unix))]
fn silence_stderr() {}

#[cfg(not(unix))]
fn restore_stderr() {}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        Clear(ClearType::All),
        EnableMouseCapture
    )?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) {
    let _ = disable_raw_mode();
    let _ = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    );
    let _ = terminal.show_cursor();
    restore_stderr();
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    silence_stderr();

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        restore_stderr();
        default_hook(info);
    }));

    let mut terminal = setup_terminal()?;
    let mut app = App::new(false, cli.search, cli.local);
    terminal.clear()?;

    let tick_rate = Duration::from_millis(33);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| ui::render(f, &mut app))?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat =>
                {
                    app.handle_key(key);
                }
                Event::Mouse(mouse) => {
                    app.handle_mouse(mouse);
                }
                _ => {}
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.tick();
            last_tick = Instant::now();
        }

        if app.should_quit {
            break;
        }
    }

    app.audio.stop();
    restore_terminal(&mut terminal);
    Ok(())
}
