//! treetop: an htop-style view of a treehouse worktree pool, for seeing which
//! agent works in each tree and what it serves, talking to them and to the
//! assistant that oversees them, and marking trees and returning or
//! destroying them. Run it from anywhere in a pooled repository. Chats and
//! diffs open in popups over the pool; `t` goes to the tree itself, a tmux
//! pane or window inside tmux, else a shell that comes back to treetop when
//! it exits. With arguments, it runs one of the commands in `cli` instead.

mod actions;
mod agents;
mod app;
mod assistant;
mod base;
mod branch;
mod builtin;
mod cache;
mod chat;
mod child;
mod cli;
mod coderabbit;
mod config;
mod diff;
mod dispatch;
mod github_plugin;
mod guard;
mod handoff;
mod hook;
mod inbox;
mod jobs;
mod keel;
mod keeper;
mod live;
mod naming;
mod nomistakes_plugin;
mod phone;
mod plugin;
mod pool;
mod popup;
mod pr_view;
mod refresh;
mod reports;
mod scratch;
mod serve;
mod ship;
mod sidecar;
mod snapshot;
mod state;
mod syntax;
mod theme;
mod tickets;
mod tmux;
mod ui;
mod update;
mod watch;
mod workspaces;

use std::io::{self, Stdout, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, Result};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind, MouseButton, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Position, Rect};

use crate::app::{App, Choice, Opening, Outcome};
use crate::chat::{Chat, Chats};
use crate::dispatch::Recipient;
use crate::jobs::{Kind, Worker};
use crate::keeper::Keeper;
use crate::refresh::{Refresher, Update};

type Term = Terminal<CrosstermBackend<Stdout>>;

const TICK: Duration = Duration::from_millis(100);
/// The tick while a chat is open, so what is typed echoes without a lag.
const CHAT_TICK: Duration = Duration::from_millis(16);
/// How long the cursor rests on a tree before the window under the table
/// opens its chat, so moving down the list starts no chat on the way.
const WINDOW_REST: Duration = Duration::from_millis(300);
/// How long before the second window of a double tries again to start a
/// chat that would not start.
const OTHER_RETRY: Duration = Duration::from_secs(5);
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

/// The size of the terminal a tree's chat runs in: the window under the
/// table, under its tab bar. Where the screen is too short for that window,
/// the size a popup chat gets.
fn chat_size(term: &Term, app: &App) -> Result<(u16, u16)> {
    let size = term.size()?;
    let screen = Rect::new(0, 0, size.width, size.height);
    Ok(match ui::window_chat_area(app, screen) {
        Some(area) => (area.height.max(1), area.width.max(1)),
        None => popup_size(term, app)?,
    })
}

/// The size of the terminal a popup chat runs in, the assistant's or a
/// scratch one: its popup under the bar, less its sidebar, on the screen
/// under the banner.
fn popup_size(term: &Term, app: &App) -> Result<(u16, u16)> {
    let size = term.size()?;
    let stage = ui::stage(app, Rect::new(0, 0, size.width, size.height));
    Ok(popup::terminal_size(stage.height, stage.width))
}

/// A chat takes the keys: one in a popup, or the window's while it has
/// them on its chat or shell tab.
fn chat_has_keys(chats: &Chats, app: &App) -> bool {
    // The palette takes the keys over any chat while it is open.
    app.palette.is_none()
        && chats
            .open()
            .is_some_and(|chat| ui::is_popup_chat(chat) || app.is_inserting())
}

/// The chat open is the one the window's tab shows for its tree: its
/// agent's on the chat tab, whichever agent, and its shell on the shell tab.
fn is_window_chat(chat: &Chat, app: &App) -> bool {
    let is_here = app.window_tree().map(|t| t.name.as_str()) == Some(chat.tree.as_str());
    let is_shell = chat.agent == app::SHELL;
    is_here
        && match app.shown_tab() {
            app::WindowTab::Chat => !is_shell,
            app::WindowTab::Shell => is_shell,
            _ => false,
        }
}

/// Opens an agent's chat with the keys: a tree's in the window under the
/// table, its tree under the cursor, and the assistant's or a scratch one
/// in a popup over treetop. Says why where it could not.
fn open_chat(
    term: &Term,
    app: &mut App,
    chats: &mut Chats,
    command: &[String],
    agent: String,
    tree: String,
) -> Result<()> {
    let is_popup = ui::is_popup(&agent, &tree);
    if !is_popup {
        app.select_tree(&tree);
        app.tab = app::WindowTab::Chat;
        if app.window_tree().map(|t| t.name.as_str()) != Some(tree.as_str()) {
            app.message = Some(format!(
                "tree {tree} is out of the list; open its group or clear the filter to see its chat"
            ));
            return Ok(());
        }
    }
    let size = if is_popup {
        popup_size(term, app)?
    } else {
        chat_size(term, app)?
    };
    let hints = app
        .trees
        .iter()
        .flat_map(|t| &t.agents)
        .find(|a| a.attach.as_deref() == Some(command))
        .and_then(|a| a.plugin.as_deref())
        .and_then(plugin::screen_of);
    if chats.open().is_some_and(|open| open.command() != command) {
        chats.hide();
    }
    match chats.show(command, size, agent, tree, hints) {
        // The mouse is captured while a chat has the keys, so the wheel
        // scrolls it; on the list, the terminal keeps the mouse to itself.
        // A tree's chat opens in Normal, so a second Enter starts Insert
        // and nothing typed reaches the agent by accident.
        Ok(()) => {
            app.is_window_focused = !is_popup;
            app.is_inserting = false;
            execute!(io::stdout(), EnableMouseCapture)?;
        }
        Err(err) => app.message = Some(format!("{err:#}")),
    }
    Ok(())
}

/// Shows the chat of the tree under the cursor in the window under the
/// table, without the keys, once the cursor has rested on it.
fn show_in_window(
    term: &Term,
    app: &App,
    chats: &mut Chats,
    (command, agent, tree): (Vec<String>, String, String),
) -> Result<()> {
    let size = chat_size(term, app)?;
    let hints = app.window_plugin().and_then(plugin::screen_of);
    // A chat that cannot open says so in the window, as an opening note
    // that stays; the next cursor move tries another.
    let _ = chats.show(&command, size, agent, tree, hints);
    Ok(())
}

/// Gives the mouse back to the terminal once no chat has the keys.
fn chat_closed(app: &mut App, agent: &str, tree: &str) -> Result<()> {
    app.message = Some(format!("back from {agent} in tree {tree}"));
    leave_window(app)
}

/// The window's chat gives the keys back to the table and keeps running
/// on screen.
fn leave_window(app: &mut App) -> Result<()> {
    app.is_window_focused = false;
    execute!(io::stdout(), DisableMouseCapture)?;
    Ok(())
}

/// Rings the terminal bell, which tmux passes on from whatever window you
/// are in.
fn ring() -> Result<()> {
    let mut out = io::stdout();
    out.write_all(b"\x07")?;
    out.flush()?;
    Ok(())
}

/// Rings the bell and, inside tmux, shows the news in the status line for
/// a moment: the assistant's answer, which is no tree's, so never waits.
fn announce(news: &[String]) -> Result<()> {
    ring()?;
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
    let size = chat_size(term, app)?;
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
    /// Show the pool of the repository checked out here instead, and open
    /// what waits in the tree at the second path, once it is read.
    Switch(PathBuf, Option<(PathBuf, inbox::Kind)>),
    /// Start the treetop installed over this one, on this workspace.
    Restart,
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
    let away = inbox::spawn_reader(checkout.to_path_buf());
    app.base = diff::base(checkout);
    let diffs = diff::Loader::spawn(app.base.clone());
    // Each PR the popup asks for is read on a thread of its own, since a
    // read is a few `gh` calls and the popup stays live meanwhile.
    let (prs_done, prs) = std::sync::mpsc::channel();
    // The Issues screen, made the first time it is shown.
    let mut issues: Option<issues::Screen> = None;
    let mut was_issues = false;
    // Drawn only when something changed, or every REDRAW: drawing the whole
    // table ten times a second is most of what treetop costs while idle.
    let mut is_stale = true;
    let mut drawn_at = Instant::now();
    let mut chats = Chats::default();
    let mut keeper = Keeper::default();
    // Whether the mouse is captured for an open issue or the banner, apart
    // from a chat's.
    let mut is_issue_mouse = false;
    // The sizes the chats' terminals were last given: the window's, and the
    // popup's for the one open in a popup.
    let mut chat_sized: Option<((u16, u16), (u16, u16))> = None;
    // The tree and tab the window under the table shows, and since when:
    // its chat, diff or PR opens once the cursor has rested there for
    // WINDOW_REST.
    let mut window_wants: (Option<(PathBuf, app::WindowTab)>, Instant) = (None, Instant::now());
    // The second window's chat last started behind the screen, and when.
    let mut other_tried: (Option<Vec<String>>, Instant) = (None, Instant::now());
    // A scratch chat starting, and those being deleted, off this thread.
    let mut starting_scratch: Option<mpsc::Receiver<Result<scratch::Scratch, String>>> = None;
    let mut removing_scratch: Vec<mpsc::Receiver<Result<String, String>>> = Vec::new();
    // URLs a configured browser is opening, off this thread, each with
    // whether the Issues screen asked, so its outcome is said there.
    let mut opening_urls: Vec<(actions::Pending, bool)> = Vec::new();
    // The port `p` is sharing with the phone, or `P` publishing, off this
    // thread.
    let mut sharing: Option<phone::Sharing> = None;
    // Ports being taken off the phone and the internet, each saying what
    // came of it.
    let mut unsharing: Vec<mpsc::Receiver<String>> = Vec::new();
    // When `treetop status` was last written for this workspace.
    let mut snapshots = snapshot::Clock::default();
    // `U` installing a newer release, off this thread.
    let mut updating: Option<mpsc::Receiver<Result<String, String>>> = None;
    // The line tmux's status shows for what waits on you, as last set.
    let mut shown_waiting: Option<Option<String>> = None;
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
                Update::Processes(processes) => {
                    app.apply_processes(&processes);
                    app.replaced = update::is_replaced();
                }
                Update::Work(work) => app.apply_work(&work),
                Update::Reports(reports) => {
                    // Another workspace leases trees as its agents start, so
                    // its cache is read again to place their reports.
                    app.pools = inbox::other_pools(checkout);
                    let news = app.apply_reports(reports);
                    if !news.is_empty() {
                        announce(&news)?;
                    }
                }
                Update::Sidecars(sidecars) => app.apply_sidecars(sidecars),
                Update::Ship(answer) => {
                    app.apply_ship(answer);
                    for job in app.ready_landings() {
                        worker.push(job);
                    }
                }
                Update::Usage(usage) => app.usage = usage,
                Update::Runs(runs) => app.runs = runs.unwrap_or_default(),
                Update::Base(standing) => app.apply_base(standing),
                Update::Release(tag) => app.apply_release(tag),
                Update::Shares(shares) => app.apply_shares(shares),
                Update::CatchUpHookFailed(why) => app.message = Some(why),
                Update::Agents(agents) => {
                    app.apply_agents(&agents);
                    let live: Vec<&Vec<String>> = agents
                        .values()
                        .flatten()
                        .filter_map(|a| a.attach.as_ref())
                        .collect();
                    // A shell is no agent's, so it stays while its tree does.
                    chats.retain(|chat| {
                        live.iter().any(|c| c.as_slice() == chat.command())
                            || chat.agent == app::SHELL && app.keeps_shell(&chat.tree)
                    });
                    keeper.keep(app.kept());
                }
            }
        }
        while let Ok(event) = worker.events.try_recv() {
            is_stale = true;
            match event {
                jobs::Event::Started(job) => app.job_started(&job),
                jobs::Event::Step { key, step } => app.job_step(key, step),
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
        if let Some(result) = updating.as_ref().and_then(|r| r.try_recv().ok()) {
            updating = None;
            app.is_updating = false;
            match result {
                Ok(_) if update::is_replaced().is_some() && app.active_jobs() == 0 => {
                    return Ok(Exit::Restart);
                }
                Ok(done) => app.message = done.lines().next().map(str::to_string),
                Err(why) => app.message = Some(format!("update failed: {why}")),
            }
            is_stale = true;
        }
        if app.is_read() && app.is_work_read && snapshots.is_due() {
            let ready: Vec<String> = app.ready.iter().cloned().collect();
            let status = cli::status_json(&app.trees, &app.reports, &ready, SystemTime::now());
            snapshot::write(checkout, &status);
        }
        if let Some(Outcome::Send { to, text }) = app.ready_failure() {
            send(term, app, &mut chats, &worker, to, &text)?;
            refresher.refresh_ship();
        }
        // A handoff is read once its agent has written it and stopped, and
        // taken out of the tree, so the new agent never finds it there.
        if !chat_has_keys(&chats, app)
            && let Some(file) = app.wanted_handoff()
            && let Ok(text) = std::fs::read_to_string(&file)
        {
            let _ = std::fs::remove_file(&file);
            app.handoff_written(text);
            is_stale = true;
        }
        while let Ok((path, diff)) = diffs.results.try_recv() {
            is_stale = true;
            app.diff_loaded(path, diff);
        }
        if let Some((path, base)) = app.wanted_diff() {
            diffs.request(path, base);
        }
        while let Ok((path, pr)) = prs.try_recv() {
            is_stale = true;
            app.pr_loaded(path, pr);
        }
        if let Some((path, branch, plugin)) = app.wanted_pr() {
            let done = prs_done.clone();
            std::thread::spawn(move || {
                let pr = ship::view(&plugin, &path, &branch);
                let _ = done.send((path, pr));
            });
            is_stale = true;
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
        // An open issue takes the mouse, for its links and the wheel, and
        // the list does while the banner is up, for a click on it; a chat
        // captures it on its own, and gives it back when it closes.
        if chat_has_keys(&chats, app) {
            is_issue_mouse = false;
        } else {
            let wants = if app.is_issues {
                issues.as_ref().is_some_and(|s| s.wants_mouse())
            } else {
                app.wants_mouse()
            };
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
        if let Some(result) = starting_scratch.as_ref().and_then(|r| r.try_recv().ok()) {
            starting_scratch = None;
            is_stale = true;
            match result {
                // Esc while it started leaves it for C to open.
                Ok(chat) if app.opening.is_none() => {
                    app.message = Some(format!("{} is ready; C opens it", chat.name));
                }
                Ok(chat) => {
                    app.opening = None;
                    open_chat(
                        term,
                        app,
                        &mut chats,
                        &chat.attach(),
                        chat.name,
                        app::SCRATCH.into(),
                    )?;
                }
                // Said in the list it was asked from, whole, since the bar
                // would cut it short.
                Err(err) => {
                    app.opening = None;
                    app.open_scratch_menu(scratch::list());
                    if let Some(menu) = app.scratch_menu.as_mut() {
                        menu.error = Some(format!("could not start a scratch chat: {err}"));
                    }
                }
            }
        }
        opening_urls.retain(|(pending, from_issues)| {
            let Some(said) = pending.poll() else {
                return true;
            };
            match issues.as_mut().filter(|_| *from_issues) {
                Some(screen) => screen.say(said),
                None => app.message = Some(said),
            }
            is_stale = true;
            false
        });
        if let Some(share) = &sharing
            && let Some(shared) = share.poll()
        {
            // A public link is for handing to someone, so it goes straight
            // to the clipboard.
            if share.is_public
                && let Ok(url) = &shared
            {
                app.message = Some(match actions::copy(url) {
                    Ok(()) => "link copied".to_string(),
                    Err(err) => format!("{err:#}"),
                });
            }
            app.set_shared(share.port, shared);
            sharing = None;
            // The PORTS column marks what is shared, so it reads it back
            // rather than waiting out the tick.
            refresher.refresh_shares();
            is_stale = true;
        }
        unsharing.retain(|result| match result.try_recv() {
            Ok(said) => {
                app.message = Some(said);
                refresher.refresh_shares();
                is_stale = true;
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => false,
        });
        removing_scratch.retain(|result| match result.try_recv() {
            Ok(Ok(name)) => {
                app.message = Some(format!("deleted {name}"));
                is_stale = true;
                false
            }
            Ok(Err(err)) => {
                app.message = Some(format!("could not delete the scratch chat: {err}"));
                is_stale = true;
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => false,
        });
        if !chat_has_keys(&chats, app)
            && let Some(Outcome::Chat {
                command,
                agent,
                tree,
            }) = app.ready_chat()
        {
            open_chat(term, app, &mut chats, &command, agent, tree)?;
            is_stale = true;
        }
        // What waits in another workspace, gone to from here, opens its
        // agent once the pool is read, before any ask takes the screen.
        if !chat_has_keys(&chats, app) {
            match app.arrival() {
                Some(Outcome::Chat {
                    command,
                    agent,
                    tree,
                }) => {
                    open_chat(term, app, &mut chats, &command, agent, tree)?;
                    is_stale = true;
                }
                // An agent with no chat here runs in a tmux pane, which
                // arrival only offers inside tmux.
                Some(Outcome::Enter(tree)) => {
                    app.message = Some(tmux::open(&tree).unwrap_or_else(|err| format!("{err:#}")));
                    is_stale = true;
                }
                _ => {}
            }
        }
        // The other workspaces' agents and PRs, as their slower read last
        // found them.
        while let Ok(live) = away.rounds.try_recv() {
            app.away_live = live;
            is_stale = true;
        }
        // An agent asking you something gets its chat opened once nothing
        // else is on screen, the next as the last one closes.
        match chats.open() {
            // A chat read in Normal counts as seen, as one typed to does.
            Some(open)
                if chat_has_keys(&chats, app)
                    || app.is_window_focused && is_window_chat(open, app) =>
            {
                app.saw_chat(open.command())
            }
            _ => {
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
        // The window under the table follows the cursor: the chat of the
        // tree under it, once the cursor has rested there, so moving down
        // the list opens no chat on the way. Ctrl+Q with nothing open there
        // opens it at once. A chat already open on the window's tree stays,
        // whichever of its agents it is and however the listing has it, so
        // one Enter just started is not closed while it comes up.
        if !chats.open().is_some_and(ui::is_popup_chat) {
            let here = app.window_tree().map(|t| (t.path.clone(), app.shown_tab()));
            if here != window_wants.0 {
                window_wants = (here, Instant::now());
                // A double is kept as the cursor leaves it, for a restart.
                if app.other.is_some() {
                    app.save_layout();
                }
            }
            let is_rested = app.is_window_focused || window_wants.1.elapsed() >= WINDOW_REST;
            let wanted = app.window_chat();
            let is_shown_here = chats.open().is_some_and(|c| is_window_chat(c, app));
            let is_terminal_tab = matches!(
                app.shown_tab(),
                app::WindowTab::Chat | app::WindowTab::Shell
            );
            if app.is_window_focused && is_terminal_tab && wanted.is_none() && !is_shown_here {
                leave_window(app)?;
            }
            if !is_shown_here {
                if chats.open().is_some() {
                    chats.hide();
                    is_stale = true;
                }
                if let Some(wanted) = wanted.filter(|_| is_rested) {
                    show_in_window(term, app, &mut chats, wanted)?;
                    is_stale = true;
                }
            }
            if is_rested {
                app.sync_window();
            }
            // The second window of a double shows its chat or shell live,
            // run behind the one with the keys; one that will not start is
            // tried again every OTHER_RETRY rather than on every pass.
            if let Some((command, agent, tree)) = app.other_chat()
                && chats.running(&command).is_none()
                && (other_tried.0.as_ref() != Some(&command)
                    || other_tried.1.elapsed() >= OTHER_RETRY)
            {
                let size = chat_size(term, app)?;
                let hints = app.other_plugin().and_then(plugin::screen_of);
                if chats
                    .run_behind(&command, size, (agent, tree), hints)
                    .is_ok()
                {
                    is_stale = true;
                }
                other_tried = (Some(command), Instant::now());
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
        for (agent, tree, _) in &pumped.failed {
            app.handoff_unsent(agent, tree);
        }
        if let Some((agent, _)) = &pumped.kept {
            app.message = Some(format!("kept in {agent}'s chat; Ctrl+Q leaves it"));
        }
        if let Some((agent, tree)) = pumped.ended {
            if app.is_window_focused || ui::is_popup(&agent, &tree) {
                chat_closed(app, &agent, &tree)?;
            }
            is_stale = true;
        }
        // A resize, the banner coming or going, or the windows split or
        // joined, changes the room a chat has, before it is drawn there.
        let sizes = (chat_size(term, app)?, popup_size(term, app)?);
        if chat_sized != Some(sizes) {
            let ((rows, cols), (popup_rows, popup_cols)) = sizes;
            chats.resize(rows, cols);
            let size = term.size()?;
            let screen = Rect::new(0, 0, size.width, size.height);
            if let Some(open) = chats
                .open_mut()
                .filter(|c| ui::floats(app, ui::is_popup_chat(c), screen))
            {
                open.resize(popup_rows, popup_cols);
            }
            chat_sized = Some(sizes);
        }
        if is_stale && let Some((is_new, line)) = app.watch_waiting() {
            if is_new {
                ring()?;
            }
            if app.is_in_tmux && shown_waiting.as_ref() != Some(&line) {
                tmux::show_waiting(line.as_deref());
                shown_waiting = Some(line);
            }
        }
        if is_stale || drawn_at.elapsed() >= REDRAW {
            let size = term.size()?;
            app.window_rows = ui::window_chat_area(app, Rect::new(0, 0, size.width, size.height))
                .map_or(0, |area| area.height);
            let other = app
                .other_chat()
                .and_then(|(command, ..)| chats.running(&command));
            term.draw(|frame| ui::draw(frame, app, chats.open(), other, issues.as_ref()))?;
            is_stale = false;
            drawn_at = Instant::now();
        }
        if !event::poll(if chat_has_keys(&chats, app) {
            CHAT_TICK
        } else {
            TICK
        })? {
            continue;
        }
        // A key or a resize: either way the screen is drawn again.
        is_stale = true;
        let event = chat::as_ctrl_q(event::read()?);
        // Over the window's chat while the table has the keys, the wheel
        // scrolls it and a click gives it the keys.
        if let &Event::Mouse(mouse) = &event
            && !chat_has_keys(&chats, app)
            && let Some(open) = chats.open_mut()
        {
            let size = term.size()?;
            let area = ui::window_chat_area(app, Rect::new(0, 0, size.width, size.height));
            if let Some(area) = area.filter(|a| a.contains(Position::new(mouse.column, mouse.row)))
            {
                match mouse.kind {
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        open.send_mouse(mouse, area);
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        app.insert_in_window();
                        execute!(io::stdout(), EnableMouseCapture)?;
                    }
                    _ => {}
                }
                continue;
            }
        }
        if chat_has_keys(&chats, app)
            && let Some(open) = chats.open_mut()
        {
            let size = term.size()?;
            let is_popup = ui::floats(
                app,
                ui::is_popup_chat(open),
                Rect::new(0, 0, size.width, size.height),
            );
            let mut is_list_click = false;
            match &event {
                &Event::Key(key) if key.kind != KeyEventKind::Release => {
                    // Ctrl+F zooms the windows from an agent's chat; a shell
                    // keeps it, as cursor-forward and page down.
                    // From a tree's chat only: the assistant's and a scratch
                    // chat's popups belong to no tree, and would cover a
                    // command's dialog.
                    if chat::is_palette(key) && !is_popup && open.agent != app::SHELL {
                        app.open_palette();
                    } else if chat::is_zoom(key)
                        && !ui::is_popup_chat(open)
                        && open.agent != app::SHELL
                    {
                        app.toggle_zoom(app::Zoom::Windows);
                    } else if chat::is_exit(key) && !is_popup {
                        // Ctrl+Q drops Insert to Normal, the window keeping
                        // the keys and the mouse, for the wheel and a click.
                        app.is_inserting = false;
                    } else if chat::is_exit(key) && !ui::is_popup_chat(open) {
                        // A tree's chat floating over the Issues screen or a
                        // short terminal has no Normal to drop to on screen.
                        app.cycle_keys();
                        if !app.is_window_focused {
                            leave_window(app)?;
                        }
                    } else if chat::is_exit(key) {
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
                Event::Paste(text) => open.paste(text),
                &Event::Mouse(mouse) => {
                    let size = term.size()?;
                    let screen = Rect::new(0, 0, size.width, size.height);
                    // A click on the banner over the chat leaves the chat
                    // for what it names, which the list's own click below
                    // opens.
                    let area = if is_popup {
                        Some(popup::chat(ui::stage(app, screen)))
                    } else {
                        ui::window_chat_area(app, screen)
                    };
                    let is_click = mouse.kind == MouseEventKind::Down(MouseButton::Left);
                    let is_over =
                        area.is_some_and(|a| a.contains(Position::new(mouse.column, mouse.row)));
                    if is_popup {
                        is_list_click = is_click && ui::is_on_banner(app, screen, mouse);
                    } else {
                        // A click outside the window gives the table the
                        // keys, and lands there as a click on the list.
                        is_list_click = is_click && !is_over;
                    }
                    if is_list_click && is_popup {
                        let (agent, tree) = (open.agent.clone(), open.tree.clone());
                        chats.hide();
                        chat_closed(app, &agent, &tree)?;
                    } else if is_list_click {
                        leave_window(app)?;
                    } else if let Some(area) = area {
                        open.send_mouse(mouse, area);
                    }
                }
                _ => {}
            }
            if !is_list_click {
                continue;
            }
        }
        let outcome = if let Event::Mouse(mouse) = event {
            if let Some(screen) = issues.as_mut().filter(|_| app.is_issues) {
                screen.handle_mouse(mouse);
                is_stale = true;
                continue;
            }
            let size = term.size()?;
            let screen = Rect::new(0, 0, size.width, size.height);
            let is_wheel = matches!(
                mouse.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            );
            if is_wheel
                && ui::window_chat_area(app, screen)
                    .is_some_and(|a| a.contains(Position::new(mouse.column, mouse.row)))
            {
                app.wheel_window(mouse.kind == MouseEventKind::ScrollDown);
                continue;
            }
            match ui::click(app, screen, mouse) {
                Some(outcome) => outcome,
                None => continue,
            }
        } else {
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
            match on_issues {
                Some(screen) => match screen.handle_key(key) {
                    issues::Action::None => Outcome::Continue,
                    issues::Action::Back => {
                        app.is_issues = false;
                        app.stop_attaching();
                        Outcome::Continue
                    }
                    issues::Action::Quit => app.quit(),
                    issues::Action::Pick(issue) => {
                        app.pick(issue.id, screen.source_name().to_string())
                    }
                    issues::Action::Ask(context) => {
                        app.open_ask(Some(context));
                        Outcome::Continue
                    }
                    issues::Action::Attach(issue) => {
                        if let Some((tree, record)) =
                            app.attach(issue.id, screen.source_name().to_string())
                            && let Err(err) = sidecar::write(&tree, &record)
                        {
                            app.message = Some(format!("could not record the issue: {err}"));
                        }
                        Outcome::Continue
                    }
                    issues::Action::Open(url) => {
                        let opening = actions::open_url(&url);
                        screen.say(opening.said_now());
                        if let actions::Opening::Pending(pending) = opening {
                            opening_urls.push((pending, true));
                        }
                        Outcome::Continue
                    }
                },
                None => app.handle_key(key),
            }
        };
        match outcome {
            Outcome::Continue => {}
            Outcome::Quit => return Ok(Exit::Quit),
            Outcome::Workspaces => app.open_picker(choices()),
            Outcome::Scratches => app.open_scratch_menu(scratch::list()),
            Outcome::NewScratch => {
                app.opening = Some(Opening {
                    agent: "a scratch chat".into(),
                    tree: app::SCRATCH.into(),
                    key: None,
                    failed: None,
                    leases: false,
                    side: None,
                });
                let (done, result) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ = done.send(scratch::start());
                });
                starting_scratch = Some(result);
            }
            Outcome::RemoveScratch(chat) => {
                let (done, result) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ = done.send(scratch::remove(&chat).map(|()| chat.name));
                });
                removing_scratch.push(result);
            }
            Outcome::Switch(path) => return Ok(Exit::Switch(path, None)),
            Outcome::SwitchTo {
                checkout,
                tree,
                kind,
            } => return Ok(Exit::Switch(checkout, Some((tree, kind)))),
            Outcome::Restart => return Ok(Exit::Restart),
            Outcome::Update => {
                app.is_updating = true;
                let (done, result) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ =
                        done.send(update::install(None, &mut |_| {}).map_err(|e| format!("{e:#}")));
                });
                updating = Some(result);
            }
            Outcome::Chat {
                command,
                agent,
                tree,
            } => open_chat(term, app, &mut chats, &command, agent, tree)?,
            Outcome::ScrollChat(lines) => {
                let size = term.size()?;
                let area = ui::window_chat_area(app, Rect::new(0, 0, size.width, size.height));
                if let Some(area) = area
                    && let Some(open) = chats.open_mut().filter(|c| is_window_chat(c, app))
                {
                    open.scroll(lines, area);
                }
            }
            Outcome::Record { path, record } => {
                if let Err(err) = sidecar::write(&path, &record) {
                    app.message = Some(format!("could not record it: {err}"));
                }
            }
            Outcome::OpenUrl(url) => {
                let opening = actions::open_url(&url);
                app.message = Some(opening.said_now());
                if let actions::Opening::Pending(pending) = opening {
                    opening_urls.push((pending, false));
                }
            }
            Outcome::Share { port, is_public } => {
                let config = config::load().unwrap_or_default();
                sharing = Some(if is_public {
                    phone::publish(&config.publish, port)
                } else {
                    phone::start(&config.share, port)
                });
            }
            Outcome::Unshare(port) => {
                let unpublish = config::load().map(|c| c.unpublish).unwrap_or_default();
                unsharing.push(phone::unshare(&unpublish, port));
            }
            Outcome::Browse { port, tree } => {
                let opening = actions::browse(port, &tree);
                app.message = Some(opening.said_now());
                if let actions::Opening::Pending(pending) = opening {
                    opening_urls.push((pending, false));
                }
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
            Outcome::Editor => {
                let text = app
                    .prompt
                    .as_ref()
                    .map(|p| p.text.clone())
                    .unwrap_or_default();
                let edited = edit_in_editor(term, &text);
                match (edited, app.prompt.as_mut()) {
                    (Ok(edited), Some(prompt)) => {
                        prompt.text = edited.trim_end().to_string();
                        prompt.is_from_top = true;
                    }
                    (Err(err), _) => app.message = Some(format!("could not edit it: {err}")),
                    (Ok(_), None) => {}
                }
            }
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
/// pool's chat clients leaves its agents running. Returns the workspace to
/// restart on, when `U` asked for the newer treetop.
fn show(
    term: &mut Term,
    start: PathBuf,
    mut checkout: PathBuf,
    is_issues: bool,
) -> Result<Option<PathBuf>> {
    let mut here = Some(start);
    // What waits, as the last workspace looked, so `e` keeps its place.
    let mut watch = app::Watch::default();
    let mut is_issues = is_issues;
    let mut arriving = None;
    loop {
        std::env::set_current_dir(&checkout)?;
        workspaces::record(&checkout);
        let mut app = App::new(here.take());
        app.keep_watch(std::mem::take(&mut watch));
        app.is_in_tmux = tmux::is_inside();
        app.checkout = checkout.clone();
        app.user = branch::user();
        app.scratch_root = scratch::root();
        if let Some(file) = state::asked_file(&checkout) {
            app.keep_asked_in(file);
        }
        if let Some(file) = state::layout_file(&checkout) {
            app.keep_layout_in(file);
        }
        if let Some(file) = inbox::file() {
            app.keep_inbox_in(file);
        }
        app.arriving = arriving.take();
        app.pools = inbox::other_pools(&checkout);
        app.ship_plugins = ship::plugins(&checkout).0;
        app.is_issues = std::mem::take(&mut is_issues);
        if let Some(mut cached) = cache::read(&checkout) {
            cached.insert(0, pool::main_tree(&checkout));
            app.set_cached(cached);
        }
        let exit = run(term, &mut app, &checkout);
        watch = app.take_watch();
        // Leaving treetop takes what waits off tmux's status line; a switch
        // shows the next workspace's at once.
        if app.is_in_tmux && !matches!(exit, Ok(Exit::Switch(..))) {
            tmux::show_waiting(None);
        }
        match exit? {
            Exit::Quit => return Ok(None),
            Exit::Switch(next, tree) => {
                checkout = next;
                arriving = tree;
            }
            Exit::Restart => return Ok(Some(checkout)),
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
    // Claude Code runs its hooks wherever a session stands, in a repository
    // or not.
    if let [command, event] = args.as_slice()
        && command == "hook"
    {
        return hook::run(event);
    }
    // `treetop update` needs no repository, since it replaces the binary.
    if let [command, rest @ ..] = args.as_slice()
        && command == "update"
        && rest.len() <= 1
    {
        return update::run(rest.first().map(String::as_str));
    }
    // `treetop serve` serves the pool treetop would open here, as the
    // screen does from outside any repository.
    if let [command, rest @ ..] = args.as_slice()
        && command == "serve"
    {
        if rest == ["--uninstall"] {
            return serve::uninstall();
        }
        let checkout = checkout_for(&start, true)?;
        std::env::set_current_dir(&checkout)?;
        return serve::run(rest, checkout);
    }
    // `treetop issues` opens the screen on the Issues screen.
    let is_issues = args == ["issues"];
    let checkout = checkout_for(&start, args.is_empty() || is_issues)?;
    std::env::set_current_dir(&checkout)?;
    if !args.is_empty() && !is_issues {
        return cli::run(&args, &checkout, &start);
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = leave_screen();
        default_hook(info);
    }));

    let mut term = enter_screen()?;
    let result = show(&mut term, start, checkout, is_issues);
    leave_screen()?;
    match result? {
        Some(workspace) => restart(&workspace),
        None => Ok(()),
    }
}

/// Becomes the treetop installed over this one, opened on `workspace`. Only
/// returns when it could not start.
fn restart(workspace: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt as _;
    let exe = update::is_replaced().context("the newer treetop is gone")?;
    let err = std::process::Command::new(&exe)
        .current_dir(workspace)
        .exec();
    Err(err).with_context(|| format!("starting {}", exe.display()))
}
