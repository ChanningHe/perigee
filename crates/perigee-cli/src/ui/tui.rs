use anyhow::Result;
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::CrosstermBackend;
use ratatui::{DefaultTerminal, Terminal};

/// RAII guard that restores the terminal on drop. This covers every exit path
/// of the TUI uniformly: a normal return, an early `?` error, or a panic
/// unwinding through the render/input loop. Without it a panic skips restore
/// and leaves the terminal in raw mode + alternate screen, i.e. unusable.
pub struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

/// Enter raw mode + alternate screen and return the terminal together with a
/// guard. Hold the guard for the lifetime of the TUI; dropping it restores the
/// terminal. A panic hook is installed so a panic restores the terminal before
/// the default handler prints, keeping the message visible on the normal screen.
pub fn init() -> Result<(DefaultTerminal, TerminalGuard)> {
    set_panic_hook();
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    let terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    Ok((terminal, TerminalGuard))
}

/// Restore the terminal to a sane state. Best-effort: errors are swallowed
/// because this runs on cleanup/panic paths where there is nothing to recover.
fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
}

fn set_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        original(info);
    }));
}
