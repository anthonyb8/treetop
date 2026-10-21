//! treetop: an htop-style view of a treehouse worktree pool, for seeing which
//! agent works in each tree and what it serves, talking to them and to the
//! coordinator that oversees them, and marking trees and returning or
//! destroying them. Run it from anywhere in a pooled repository. Chats and
//! diffs open in popups over the pool; `t` goes to the tree itself, a tmux
//! pane or window inside tmux, else a shell that comes back to treetop when
//! it exits. With arguments, it runs one of the commands in `cli` instead.

mod actions;
mod agents;
mod app;
mod branch;
mod builtin;
mod cache;
mod chat;
mod child;
mod cli;
mod config;
mod coordinator;
mod diff;
mod dispatch;
mod github_plugin;
mod jobs;
mod keeper;
mod linear_plugin;
mod live;
mod nomistakes_plugin;
mod plugin;
mod pool;
mod popup;
mod refresh;
mod reports;
mod ship;
mod sidecar;
mod theme;
mod tickets;
mod tmux;
mod ui;
mod watch;
mod workspaces;

use std::io::{self, Stdout, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::Rect;

use crate::app::{App, Choice, Outcome};
use crate::chat::Chats;
use crate::dispatch::Recipient;
use crate::jobs::{Kind, Worker};
use crate::keeper::Keeper;
use crate::refresh::{Refresher, Update};

type Term = Terminal<CrosstermBackend<Stdout>>;

const TICK: Duration = Duration::from_millis(100);
/// The tick while a chat is open, so what is typed echoes without a lag.
const CHAT_TICK: Duration = Duration::from_millis(16);
/// How often the screen is drawn when nothing has changed, for the listing's
/// age in the summary.
const REDRAW: Duration = Duration::from_secs(1);

/// Bracketed paste stays on throughout, so a paste into the box arrives as
/// one piece rather than as keys, where its first line break would send it.
fn enter_screen() -> Result<Term> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
    Ok(Terminal::new(CrosstermBackend::new(io::stdout()))?)
}

/// Gives the terminal back as treetop found it. The modes a chat switches
/// on are switched off here too, so leaving with a chat open, or on a
/// panic, never leaves the mouse captured.
fn leave_screen() -> Result<()> {
    disable_raw_mode()?;
    execute!(
        io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
    Ok(())
}

/// The size of the terminal a chat runs in: its popup under the bar.
fn chat_size(term: &Term) -> Result<(u16, u16)> {
    let size = term.size()?;
    Ok(popup::terminal_size(size.height, size.width))
}

/// Opens an agent's chat in a popup over treetop, or says why it could not.
fn open_chat(
    term: &Term,
    app: &mut App,
    chats: &mut Chats,
    command: &[String],
    agent: String,
    tree: String,
) -> Result<()> {
    let size = chat_size(term)?;
    let hints = app
        .trees
        .iter()
        .flat_map(|t| &t.agents)
        .find(|a| a.attach.as_deref() == Some(command))
        .and_then(|a| a.plugin.as_deref())
        .and_then(plugin::screen_of);
    match chats.show(command, size, agent, tree, hints) {
        // The mouse is captured only while a chat is open, so the wheel
        // scrolls it; on the list, the terminal keeps the mouse to itself.
        Ok(()) => execute!(io::stdout(), EnableMouseCapture)?,
        Err(err) => app.message = Some(format!("{err:#}")),
    }
    Ok(())
}

/// Gives the mouse back to the terminal once no chat is on screen.
fn chat_closed(app: &mut App, agent: &str, tree: &str) -> Result<()> {
    app.message = Some(format!("back from {agent} in tree {tree}"));
    execute!(io::stdout(), DisableMouseCapture)?;
    Ok(())
}

/// Rings the terminal bell and, inside tmux, shows the most pressing piece of
/// news in the status line, so an agent that needs you is heard from any
/// window. News comes what needs you first.
fn announce(news: &[String]) -> Result<()> {
    let mut out = io::stdout();
    out.write_all(b"\x07")?;
    out.flush()?;
    if let Some(first) = news.first()
        && tmux::is_inside()
    {
        let more = match news.len() - 1 {
            0 => String::new(),
            n => format!(" (+{n} more)"),
        };
        let line: String = first.chars().take(160).collect();
        tmux::notify(&format!("treetop: {line}{more}"));
    }
    Ok(())
}

/// Opens `text` in the user's editor, `$VISUAL`, else `$EDITOR`, else `vi`,
/// with treetop's screen given back to the terminal until it exits, and
/// returns what was saved.
fn edit_in_editor(term: &mut Term, text: &str) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!("treetop-issue-{}.md", std::process::id()));
    std::fs::write(&path, text).map_err(|err| format!("{}: {err}", path.display()))?;
    let editor = ["VISUAL", "EDITOR"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.trim().is_empty()))
        .unwrap_or_else(|| "vi".into());
    let mut words = editor.split_whitespace();
    let program = words.next().unwrap_or("vi");
    leave_screen().map_err(|err| format!("{err:#}"))?;
    let status = std::process::Command::new(program)
        .args(words)
        .arg(&path)
        .status();
    *term = enter_screen().map_err(|err| format!("{err:#}"))?;
    let saved = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    match status {
        Ok(status) if status.success() => saved.map_err(|err| err.to_string()),
        Ok(status) => Err(format!("{program} {status}")),
        Err(err) => Err(format!("{program}: {err}")),
    }
}

/// Types `text` into each recipient's chat behind the screen, or has its
/// plugin deliver it to one that is not running.
fn send(
    term: &Term,
    app: &mut App,
    chats: &mut Chats,
    worker: &Worker,
    to: Vec<Recipient>,
    text: &str,
) -> Result<()> {
    let size = chat_size(term)?;
    for recipient in to {
        if let Some(job) = app.send_cold(&recipient, text) {
            worker.push(job);
            continue;
        }
        let agent = recipient.agent.clone();
        let hints = recipient.plugin.as_deref().and_then(plugin::screen_of);
        if let Err(err) = chats.deliver(
            &recipient.command,
            text,
            size,
            (recipient.agent, recipient.tree),
            hints,
        ) {
            app.message = Some(format!("could not reach {agent}: {err:#}"));
        }
    }
    Ok(())
}

/// Why `run` stopped showing a pool.
enum Exit {
    Quit,
    /// Show the pool of the repository checked out here instead.
    Switch(PathBuf),
}

/// Every workspace, with how many trees each held at its last listing.
fn choices() -> Vec<Choice> {
    workspaces::list()
        .into_iter()
        .map(|workspace| Choice {
            held: cache::read(&workspace.checkout)
                .map(|trees| trees.iter().filter(|t| t.is_held()).count()),
            name: workspace.name(),
            checkout: workspace.checkout,
        })
        .collect()
}

fn run(term: &mut Term, app: &mut App, checkout: &Path) -> Result<Exit> {
    let refresher = Refresher::spawn(checkout.to_path_buf(), app.trees.clone());
    let worker = Worker::spawn();
    let diffs = diff::Loader::spawn(diff::base(checkout));
    // The Issues screen, made the first time it is shown.
    let mut issues: Option<issues::Screen> = None;
    let mut was_issues = false;
    // Drawn only when something changed, or every REDRAW: drawing the whole
    // table ten times a second is most of what treetop costs while idle.
    let mut is_stale = true;
    let mut drawn_at = Instant::now();
    let mut chats = Chats::default();
    let mut keeper = Keeper::default();
    // Whether the mouse is captured for an open issue, apart from a chat's.
    let mut is_issue_mouse = false;
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
                Update::Reports(reports) => {
                    let news = app.apply_reports(reports);
                    if !news.is_empty() {
                        announce(&news)?;
                    }
                }
                Update::Sidecars(sidecars) => app.apply_sidecars(sidecars),
                Update::Ship(answer) => {
                    let news = app.apply_ship(answer);
                    if !news.is_empty() {
                        announce(&news)?;
                    }
                    for job in app.ready_landings() {
                        worker.push(job);
                    }
                }
                Update::Agents(agents) => {
                    app.apply_agents(&agents);
                    let live: Vec<&Vec<String>> = agents
                        .values()
                        .flatten()
                        .filter_map(|a| a.attach.as_ref())
                        .collect();
                    chats.retain(|command| live.iter().any(|c| c.as_slice() == command));
                    keeper.keep(app.kept());
                }
            }
        }
        while let Ok(event) = worker.events.try_recv() {
            is_stale = true;
            match event {
                jobs::Event::Started(job) => app.job_started(&job),
                jobs::Event::Finished { job, is_ok, output } => {
                    let is_preview = job.kind == Kind::Preview;
                    if job.kind == Kind::Ship {
                        refresher.refresh_ship();
                    }
                    app.job_finished(job, is_ok, output);
                    if !is_preview {
                        refresher.refresh();
                    }
                }
            }
        }
        if let Some(Outcome::Send { to, text }) = app.ready_failure() {
            send(term, app, &mut chats, &worker, to, &text)?;
            refresher.refresh_ship();
        }
        while let Ok((path, diff)) = diffs.results.try_recv() {
            is_stale = true;
            app.diff_loaded(path, diff);
        }
        if let Some(path) = app.wanted_diff() {
            diffs.request(path);
        }
        if app.is_issues {
            match issues.as_mut() {
                None => match tickets::backends(checkout) {
                    Ok((backends, Ok(at))) => {
                        issues = Some(issues::Screen::new(backends, at, tickets::views()));
                    }
                    Ok((backends, Err(why))) => {
                        issues = Some(issues::Screen::without_source(
                            backends,
                            why,
                            tickets::views(),
                        ));
                    }
                    Err(err) => {
                        app.is_issues = false;
                        app.message = Some(err);
                    }
                },
                Some(screen) if !was_issues => screen.open(),
                Some(_) => {}
            }
            if let Some(screen) = issues.as_mut() {
                if let Some(id) = app.open_issue.take() {
                    screen.show_issue(&id);
                }
                if let Some(tree) = app.choose_issue_for.take() {
                    screen.choose_issue(format!("the issue tree {tree} works on"));
                }
                is_stale |= screen.poll();
                // Asked only once an image is wanted, since a terminal that
                // never answers holds the screen up while it is asked.
                if screen.wants_terminal_asked() {
                    screen.ask_terminal_for_images();
                    is_stale = true;
                }
                if let Some(text) = screen.take_editor() {
                    refresher.set_paused(true);
                    let edited = edit_in_editor(term, &text);
                    refresher.set_paused(false);
                    screen.edited(edited);
                    is_stale = true;
                }
            }
        }
        was_issues = app.is_issues;
        // An open issue takes the mouse, for its links and the wheel; a
        // chat captures it on its own, and gives it back when it closes.
        if chats.is_open() {
            is_issue_mouse = false;
        } else {
            let wants = app.is_issues && issues.as_ref().is_some_and(|s| s.wants_mouse());
            if wants != is_issue_mouse {
                if wants {
                    execute!(io::stdout(), EnableMouseCapture)?;
                } else {
                    execute!(io::stdout(), DisableMouseCapture)?;
                }
                is_issue_mouse = wants;
            }
        }
        if app.is_quitting && app.active_jobs() == 0 {
            return Ok(Exit::Quit);
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
        // An agent asking you something gets its chat opened once nothing
        // else is on screen, the next as the last one closes.
        match chats.open() {
            Some(open) => app.saw_chat(open.command()),
            None => {
                if let Some(Outcome::Chat {
                    command,
                    agent,
                    tree,
                }) = app.next_ask()
                {
                    open_chat(term, app, &mut chats, &command, agent, tree)?;
                    is_stale = true;
                }
            }
        }
        let pumped = chats.pump();
        is_stale |= pumped.is_stale;
        if let Some((agent, tree)) = pumped.delivered.last() {
            app.message = Some(match pumped.delivered.len() {
                1 => format!("sent to {agent} in {tree}"),
                n => format!("sent to {n} agents"),
            });
            is_stale = true;
        }
        if let Some((agent, tree, why)) = pumped.failed.first() {
            app.message = Some(format!("not sent to {agent} in {tree}: {why}"));
            is_stale = true;
        }
        if let Some((agent, _)) = &pumped.kept {
            app.message = Some(format!(
                "kept in {agent}'s chat; Ctrl+Q goes back to the list"
            ));
        }
        if let Some((agent, tree)) = pumped.ended {
            chat_closed(app, &agent, &tree)?;
            is_stale = true;
        }
        if is_stale || drawn_at.elapsed() >= REDRAW {
            app.screen_height = term.size()?.height;
            term.draw(|frame| ui::draw(frame, app, chats.open(), issues.as_ref()))?;
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
            let (rows, cols) = popup::terminal_size(rows, cols);
            chats.resize(rows, cols);
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
                Event::Mouse(mouse) => {
                    let size = term.size()?;
                    let screen = Rect::new(0, 0, size.width, size.height);
                    open.send_mouse(mouse, popup::body(screen));
                }
                _ => {}
            }
            continue;
        }
        if let Event::Mouse(mouse) = &event {
            if let Some(screen) = issues.as_mut().filter(|_| app.is_issues) {
                screen.handle_mouse(*mouse);
                is_stale = true;
            }
            continue;
        }
        if let Event::Paste(text) = &event {
            match issues.as_mut().filter(|_| app.is_issues) {
                Some(screen) => screen.paste(text),
                None => app.paste(text),
            }
            continue;
        }
        let Event::Key(key) = event else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let on_issues = issues
            .as_mut()
            .filter(|_| app.is_issues && app.prompt.is_none());
        let outcome = match on_issues {
            Some(screen) => match screen.handle_key(key) {
                issues::Action::None => Outcome::Continue,
                issues::Action::Back => {
                    app.is_issues = false;
                    app.stop_attaching();
                    Outcome::Continue
                }
                issues::Action::Quit => app.quit(),
                issues::Action::Pick(issue) => app.pick(issue.id),
                issues::Action::Ask(context) => {
                    app.open_ask(Some(context));
                    Outcome::Continue
                }
                issues::Action::Attach(issue) => {
                    if let Some((tree, record)) = app.attach(issue.id)
                        && let Err(err) = sidecar::write(&tree, &record)
                    {
                        app.message = Some(format!("could not record the issue: {err}"));
                    }
                    Outcome::Continue
                }
                issues::Action::Open(url) => {
                    screen.say(actions::open_url(&url).unwrap_or_else(|err| format!("{err:#}")));
                    Outcome::Continue
                }
            },
            None => app.handle_key(key),
        };
        match outcome {
            Outcome::Continue => {}
            Outcome::Quit => return Ok(Exit::Quit),
            Outcome::Workspaces => app.open_picker(choices()),
            Outcome::Switch(path) => return Ok(Exit::Switch(path)),
            Outcome::Chat {
                command,
                agent,
                tree,
            } => open_chat(term, app, &mut chats, &command, agent, tree)?,
            Outcome::Record { path, record } => {
                if let Err(err) = sidecar::write(&path, &record) {
                    app.message = Some(format!("could not record it: {err}"));
                }
            }
            Outcome::OpenUrl(url) => {
                app.message =
                    Some(actions::open_url(&url).unwrap_or_else(|err| format!("{err:#}")));
            }
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
            Outcome::Send { to, text } => send(term, app, &mut chats, &worker, to, &text)?,
            Outcome::Queue(jobs) => {
                for job in jobs {
                    worker.push(job);
                }
            }
        }
    }
}

/// The repository to open: the one `start` is in, else, for the screen, the
/// workspace opened most recently, so treetop opens from anywhere.
fn checkout_for(start: &Path, is_screen: bool) -> Result<PathBuf> {
    match pool::main_checkout(start) {
        Ok(checkout) => Ok(checkout),
        Err(err) => match workspaces::list().into_iter().next() {
            Some(last) if is_screen => Ok(last.checkout),
            _ => Err(err),
        },
    }
}

/// Shows the pool of `checkout`, and of each workspace `w` switches to,
/// until treetop quits, opening on the Issues screen when `is_issues`. Each
/// switch starts the pool's clocks, worker and chats afresh; ending the old
/// pool's chat clients leaves its agents running.
fn show(term: &mut Term, start: PathBuf, mut checkout: PathBuf, is_issues: bool) -> Result<()> {
    let mut here = Some(start);
    let mut is_issues = is_issues;
    loop {
        std::env::set_current_dir(&checkout)?;
        workspaces::record(&checkout);
        let mut app = App::new(here.take());
        app.is_in_tmux = tmux::is_inside();
        app.checkout = checkout.clone();
        app.user = branch::user();
        app.ship_plugins = ship::plugins(&checkout).0;
        app.is_issues = std::mem::take(&mut is_issues);
        if let Some(mut cached) = cache::read(&checkout) {
            cached.insert(0, pool::main_tree(&checkout));
            app.set_cached(cached);
        }
        match run(term, &mut app, &checkout)? {
            Exit::Quit => return Ok(()),
            Exit::Switch(next) => checkout = next,
        }
    }
}

fn main() -> Result<()> {
    let start = std::env::current_dir()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(answer) = cli::answer_anywhere(&args) {
        println!("{answer}");
        return Ok(());
    }
    if let [command, name] = args.as_slice()
        && command == "plugin"
    {
        return builtin::serve(name);
    }
    // `treetop issues` opens the screen on the Issues screen.
    let is_issues = args == ["issues"];
    let checkout = checkout_for(&start, args.is_empty() || is_issues)?;
    std::env::set_current_dir(&checkout)?;
    if !args.is_empty() && !is_issues {
        return cli::run(&args, &checkout);
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = leave_screen();
        default_hook(info);
    }));

    let mut term = enter_screen()?;
    let result = show(&mut term, start, checkout, is_issues);
    leave_screen()?;
    result
}
