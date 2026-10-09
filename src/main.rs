//! treetop: an htop-style view of a treehouse worktree pool, for marking trees
//! and returning or destroying them. Run it from anywhere in a pooled
//! repository; Enter opens the tree in a tmux window, or outside tmux in a
//! shell that comes back to treetop when it exits.

mod actions;
mod app;
mod cache;
mod diff;
mod jobs;
mod live;
mod pool;
mod refresh;
mod tmux;
mod ui;

use std::io::{self, Stdout};
use std::path::Path;
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
use crate::jobs::{Kind, Worker};
use crate::refresh::{Refresher, Update};

type Term = Terminal<CrosstermBackend<Stdout>>;

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

fn run(term: &mut Term, app: &mut App, checkout: &Path) -> Result<()> {
    let refresher = Refresher::spawn(checkout.to_path_buf(), app.trees.clone());
    let worker = Worker::spawn();
    let diffs = diff::Loader::spawn(diff::base(checkout));
    loop {
        while let Ok(update) = refresher.updates.try_recv() {
            match update {
                Update::ListingStarted => app.start_listing(),
                Update::Listing(Ok(listing)) => app.set_trees(listing),
                Update::Listing(Err(err)) => {
                    app.is_listing = false;
                    app.error = Some(format!("{err:#}"));
                }
                Update::Processes(processes) => app.apply_processes(&processes),
                Update::Work(work) => app.apply_work(&work),
            }
        }
        while let Ok(event) = worker.events.try_recv() {
            match event {
                jobs::Event::Started(job) => app.job_started(&job),
                jobs::Event::Finished { job, is_ok, output } => {
                    let is_preview = job.kind == Kind::Preview;
                    app.job_finished(job, is_ok, output);
                    if !is_preview {
                        refresher.refresh();
                    }
                }
            }
        }
        while let Ok((path, diff)) = diffs.results.try_recv() {
            app.diff_loaded(path, diff);
        }
        if let Some(path) = app.wanted_diff() {
            diffs.request(path);
        }
        if app.is_quitting && app.active_jobs() == 0 {
            return Ok(());
        }
        app.screen_height = term.size()?.height;
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
            Outcome::Refresh => refresher.refresh(),
            Outcome::Enter(tree) if tmux::is_inside() => {
                app.message = Some(tmux::open(&tree).unwrap_or_else(|err| format!("{err:#}")));
            }
            Outcome::Enter(tree) => {
                leave_screen()?;
                refresher.set_paused(true);
                actions::shell(&tree.path);
                refresher.set_paused(false);
                *term = enter_screen()?;
                refresher.refresh();
            }
            Outcome::Queue(jobs) => {
                for job in jobs {
                    worker.push(job);
                }
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
    if let Some(cached) = cache::read(&checkout) {
        app.set_cached(cached);
    }
    let mut term = enter_screen()?;
    let result = run(&mut term, &mut app, &checkout);
    leave_screen()?;
    result
}
