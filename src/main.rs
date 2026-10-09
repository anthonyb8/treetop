//! treetop: an htop-style view of a treehouse worktree pool, for marking trees
//! and returning or destroying them. Run it from anywhere in a pooled
//! repository; Enter opens the tree in a tmux window, or outside tmux in a
//! shell that comes back to treetop when it exits.

mod actions;
mod app;
mod pool;
mod tmux;
mod ui;

use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use crate::app::{App, Outcome};
use crate::pool::Tree;

type Term = Terminal<CrosstermBackend<Stdout>>;

const REFRESH: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_millis(100);

fn enter_screen() -> Result<Term> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(io::stdout()))?)
}

fn leave_screen() -> Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}

/// Lists the pool REFRESH after each listing finishes, or at once when kicked,
/// off the UI thread. `treehouse status` scans every process on the machine and
/// takes seconds of CPU, so a short interval would keep a core busy. Nothing is
/// listed while `paused` is set, which covers the hours a shell may sit open.
fn spawn_refresher(
    dir: PathBuf,
    paused: Arc<AtomicBool>,
) -> (Receiver<Result<Vec<Tree>>>, Sender<()>) {
    let (trees_tx, trees_rx) = mpsc::channel();
    let (kick_tx, kick_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        loop {
            if !paused.load(Ordering::Relaxed) && trees_tx.send(pool::load(&dir)).is_err() {
                return;
            }
            if let Err(RecvTimeoutError::Disconnected) = kick_rx.recv_timeout(REFRESH) {
                return;
            }
        }
    });
    (trees_rx, kick_tx)
}

fn run(term: &mut Term, app: &mut App, dir: &Path) -> Result<()> {
    let paused = Arc::new(AtomicBool::new(false));
    let (trees, kick) = spawn_refresher(dir.to_path_buf(), Arc::clone(&paused));
    loop {
        while let Ok(listing) = trees.try_recv() {
            match listing {
                Ok(listing) => app.set_trees(listing),
                Err(err) => app.error = Some(format!("{err:#}")),
            }
        }
        term.draw(|frame| ui::draw(frame, app))?;
        if !event::poll(TICK)? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match app.handle_key(key) {
            Outcome::Continue => {}
            Outcome::Quit => return Ok(()),
            Outcome::Enter(tree) if tmux::is_inside() => {
                app.message = Some(tmux::open(&tree).unwrap_or_else(|err| format!("{err:#}")));
            }
            Outcome::Enter(tree) => {
                leave_screen()?;
                paused.store(true, Ordering::Relaxed);
                actions::shell(&tree.path);
                paused.store(false, Ordering::Relaxed);
                *term = enter_screen()?;
                let _ = kick.send(());
            }
            Outcome::Run(pending) => {
                leave_screen()?;
                actions::perform(&pending);
                *term = enter_screen()?;
                let _ = kick.send(());
            }
        }
    }
}

fn main() -> Result<()> {
    let start = std::env::current_dir()?;
    let checkout = pool::main_checkout(&start)?;
    std::env::set_current_dir(&checkout)?;

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = leave_screen();
        default_hook(info);
    }));

    let mut app = App::new(Some(start));
    let mut term = enter_screen()?;
    let result = run(&mut term, &mut app, &checkout);
    leave_screen()?;
    result
}
