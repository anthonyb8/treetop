//! treetop: an htop-style view of a treehouse worktree pool, for seeing which
//! agent works in each tree and what it serves, and for marking trees and
//! returning or destroying them. Run it from anywhere in a pooled repository;
//! Enter switches to the tree's agent or a window in the tree inside tmux, or
//! outside tmux opens a shell that comes back to treetop when it exits.

mod actions;
mod agents;
mod app;
mod cache;
mod chat;
mod diff;
mod jobs;
mod live;
mod pool;
mod refresh;
mod theme;
mod tmux;
mod ui;
mod watch;

use std::io::{self, Stdout};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use crate::app::{App, Outcome};
use crate::chat::Chats;
use crate::jobs::{Kind, Worker};
use crate::refresh::{Refresher, Update};

type Term = Terminal<CrosstermBackend<Stdout>>;

const TICK: Duration = Duration::from_millis(100);
/// The tick while a chat is open, so what is typed echoes without a lag.
const CHAT_TICK: Duration = Duration::from_millis(16);
/// How often the screen is drawn when nothing has changed, for the listing's
/// age in the summary.
const REDRAW: Duration = Duration::from_secs(1);

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

/// Opens an agent's chat over treetop's screen, or says why it could not.
fn open_chat(
    term: &Term,
    app: &mut App,
    chats: &mut Chats,
    command: &[String],
    agent: String,
    tree: String,
) -> Result<()> {
    let size = term.size()?;
    match chats.show(
        command,
        size.height.saturating_sub(1),
        size.width,
        agent,
        tree,
    ) {
        Ok(()) => execute!(io::stdout(), EnableBracketedPaste)?,
        Err(err) => app.message = Some(format!("{err:#}")),
    }
    Ok(())
}

/// Switches the paste mode a chat needs off again once none is on screen.
fn chat_closed(app: &mut App, agent: &str, tree: &str) -> Result<()> {
    app.message = Some(format!("back from {agent} in tree {tree}"));
    execute!(io::stdout(), DisableBracketedPaste)?;
    Ok(())
}

fn run(term: &mut Term, app: &mut App, checkout: &Path) -> Result<()> {
    let refresher = Refresher::spawn(checkout.to_path_buf(), app.trees.clone());
    let worker = Worker::spawn();
    let diffs = diff::Loader::spawn(diff::base(checkout));
    // Drawn only when something changed, or every REDRAW: drawing the whole
    // table ten times a second is most of what treetop costs while idle.
    let mut is_stale = true;
    let mut drawn_at = Instant::now();
    let mut chats = Chats::default();
    loop {
        while let Ok(update) = refresher.updates.try_recv() {
            is_stale = true;
            match update {
                Update::ListingStarted => app.start_listing(),
                Update::Listing(Ok(listing)) => app.set_trees(listing),
                Update::Listing(Err(err)) => {
                    app.is_listing = false;
                    app.error = Some(format!("{err:#}"));
                }
                Update::Processes(processes) => app.apply_processes(&processes),
                Update::Work(work) => app.apply_work(&work),
                Update::Agents(agents) => {
                    app.apply_agents(&agents);
                    let live: Vec<&Vec<String>> = agents
                        .values()
                        .flatten()
                        .filter_map(|a| a.attach.as_ref())
                        .collect();
                    chats.retain(|command| live.iter().any(|c| c.as_slice() == command));
                }
            }
        }
        while let Ok(event) = worker.events.try_recv() {
            is_stale = true;
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
            is_stale = true;
            app.diff_loaded(path, diff);
        }
        if let Some(path) = app.wanted_diff() {
            diffs.request(path);
        }
        if app.is_quitting && app.active_jobs() == 0 {
            return Ok(());
        }
        if !chats.is_open()
            && let Some(Outcome::Chat {
                command,
                agent,
                tree,
            }) = app.ready_chat()
        {
            open_chat(term, app, &mut chats, &command, agent, tree)?;
            is_stale = true;
        }
        let pumped = chats.pump();
        is_stale |= pumped.is_stale;
        if let Some((agent, tree)) = pumped.ended {
            chat_closed(app, &agent, &tree)?;
            is_stale = true;
        }
        if is_stale || drawn_at.elapsed() >= REDRAW {
            app.screen_height = term.size()?.height;
            term.draw(|frame| ui::draw(frame, app, chats.open()))?;
            is_stale = false;
            drawn_at = Instant::now();
        }
        if !event::poll(if chats.is_open() { CHAT_TICK } else { TICK })? {
            continue;
        }
        // A key or a resize: either way the screen is drawn again.
        is_stale = true;
        let event = event::read()?;
        if let Event::Resize(cols, rows) = event {
            chats.resize(rows.saturating_sub(1), cols);
        }
        if let Some(open) = chats.open_mut() {
            match event {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if chat::is_exit(key) {
                        let (agent, tree) = (open.agent.clone(), open.tree.clone());
                        chats.hide();
                        chat_closed(app, &agent, &tree)?;
                    } else if chat::is_next(key) {
                        let from = open.tree.clone();
                        if let Some(Outcome::Chat {
                            command,
                            agent,
                            tree,
                        }) = app.next_chat(&from)
                        {
                            chats.hide();
                            open_chat(term, app, &mut chats, &command, agent, tree)?;
                        }
                    } else {
                        open.send_key(key);
                    }
                }
                Event::Paste(text) => open.paste(&text),
                _ => {}
            }
            continue;
        }
        let Event::Key(key) = event else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match app.handle_key(key) {
            Outcome::Continue => {}
            Outcome::Quit => return Ok(()),
            Outcome::Refresh => refresher.refresh(),
            Outcome::Chat {
                command,
                agent,
                tree,
            } => open_chat(term, app, &mut chats, &command, agent, tree)?,
            Outcome::Browse { port, tree } => {
                app.message =
                    Some(actions::browse(port, &tree).unwrap_or_else(|err| format!("{err:#}")));
            }
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
    app.is_in_tmux = tmux::is_inside();
    if let Some(cached) = cache::read(&checkout) {
        app.set_cached(cached);
    }
    let mut term = enter_screen()?;
    let result = run(&mut term, &mut app, &checkout);
    leave_screen()?;
    result
}
