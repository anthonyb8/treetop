//! Drawing, htop style: the pool table, a detail pane for the tree under the
//! cursor, and a key bar with the summary. A chat, a diff or the compose box
//! floats over them in a popup, with the table dimmed round it, so treetop
//! never leaves the screen. Every colour comes from `theme`.

use std::collections::HashSet;
use std::time::{Duration, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Padding, Paragraph, Row, Table, TableState, Wrap,
};

use crate::agents::{Agent, Status};
use crate::app::{
    App, DiffView, Group, Item, JobState, ListRow, LogEntry, Opening, Outcome, Pending, Phone,
    Picker, PortMenu, Reach, Review, SCRATCH, ScratchMenu, Shipping, WindowTab,
};
use crate::assistant;
use crate::chat::Chat;
use crate::coderabbit;
use crate::diff::{Change, FileDiff, Layout as DiffLayout, Row as DiffRow, Side, ViewRow};
use crate::dispatch::{Prompt, Purpose, Target};
use crate::guard;
use crate::inbox::{self, Entry, Kind};
use crate::phone::QUIET_ZONE;
use crate::plugin::{Context, Spend, Usage};
use crate::pool::{Tree, Work};
use crate::popup;
use crate::pr_view::{self, PrPopup, Tab};
use crate::reports::{self, State};
use crate::ship::{Outcome as CheckOutcome, PrView, Stage};
use crate::syntax;
use crate::theme::theme;

/// The bar's keys, most useful first: on a narrow bar the keys at the end
/// are the ones left out, and `?` lists them all.
const KEYS: [(&str, &str); 15] = [
    ("Enter", "open"),
    ("^Q", "window"),
    ("?", "help"),
    ("e", "inbox"),
    ("n", "new here"),
    ("N", "new tree"),
    ("m", "message"),
    (":", "ask"),
    ("a", "assistant"),
    ("d", "diff"),
    ("i", "issue"),
    ("r", "return"),
    ("Space", "mark"),
    ("/", "filter"),
    ("q", "quit"),
];

/// Every key on the list, for the `?` overlay, in the groups it draws:
/// the agents first, since that is what the list is for, then the trees
/// themselves and the ways round them, what they serve and where their work
/// goes, and treetop itself last. Each says what it does in a few words, so
/// four columns fit a laptop's terminal.
const HELP: [(&str, &[(&str, &str)]); 7] = [
    (
        "Agents",
        &[
            ("Enter", "open, fold heading"),
            ("^Q", "window, table"),
            ("^\\", "two windows, one"),
            ("\u{2190} \u{2192}", "chat, info tab"),
            ("n", "new agent here"),
            ("N", "agent in new tree"),
            ("m", "message agents"),
            (":", "ask the assistant"),
            ("a", "assistant's chat"),
            ("R", "resume agent"),
            ("h", "hand off work"),
            ("e E", "inbox, hide it"),
        ],
    ),
    (
        "Trees",
        &[
            ("t", "go to tree"),
            ("d", "diff vs base"),
            ("Space", "mark"),
            ("r", "return tree"),
            ("x X", "backburner, fold it"),
            ("z Z", "park, fold Parked"),
            ("b", "rename branch"),
            ("/", "filter"),
            ("Esc", "clear filter, marks"),
        ],
    ),
    (
        "Moving",
        &[("j k \u{2191}\u{2193}", "move"), ("g G", "top, bottom")],
    ),
    (
        "Ports",
        &[
            ("o", "open in browser"),
            ("p", "share to phone"),
            ("P", "public link"),
            ("Q", "show share code"),
        ],
    ),
    (
        "Ship",
        &[
            ("s", "ship branch"),
            ("v", "view PR"),
            ("O", "open PR"),
            ("f", "send CI failures"),
        ],
    ),
    (
        "Issues",
        &[
            ("Tab", "all issues"),
            ("i", "tree's issue"),
            ("I", "choose issue"),
        ],
    ),
    (
        "treetop",
        &[
            ("w", "switch workspace"),
            ("C", "scratch chats"),
            ("U", "update treetop"),
            ("?", "this help"),
            ("q", "quit"),
        ],
    ),
];

/// How many of HELP's groups go down each column of the `?` overlay, left
/// to right, so the columns come out about as tall as each other. Where
/// four do not fit, neighbours share a column: two, else one.
const HELP_COLUMNS: [usize; 4] = [1, 2, 2, 2];

/// Most useful first, as with `KEYS`; Esc stays early so the way out is
/// never the key a narrow bar drops.
const DIFF_KEYS: [(&str, &str); 8] = [
    ("j/k", "line"),
    ("Space/b", "page"),
    ("n/p", "file"),
    ("Esc", "back"),
    ("d/u", "half page"),
    ("h/l", "sideways"),
    ("g/G", "top/end"),
    ("^R", "reload"),
];

fn fg(color: Color) -> Style {
    Style::new().fg(color)
}

/// The theme as the Issues screen takes it.
pub fn palette() -> issues::Palette {
    let t = theme();
    issues::Palette {
        text: t.text,
        muted: t.muted,
        success: t.success,
        warning: t.warning,
        danger: t.danger,
        info: t.info,
        accent: t.accent,
        special: t.special,
        title: t.title,
        bar: t.bar,
        badge: t.badge,
        header: t.header,
        cursor: t.cursor,
        border: t.border,
    }
}

/// Draws the screen: the trees, or the Issues screen when it is shown,
/// with whatever is open over it.
pub fn draw(
    frame: &mut Frame,
    app: &App,
    chat: Option<&Chat>,
    other_chat: Option<&Chat>,
    issues: Option<&issues::Screen>,
) {
    // The theme's text colour is the base for every cell; widgets below only
    // add accents, so nothing falls back to a theme's dim default.
    frame.render_widget(Block::new().style(fg(theme().text)), frame.area());
    match issues.filter(|_| app.is_issues) {
        Some(screen) => screen.draw(frame, frame.area(), &palette(), app),
        None => {
            let [main, footer] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
            let windows = windows(app, main);
            let inbox = inbox_layout(app, windows.top);
            // The table has the keys unless the window or the inbox does.
            let block = window_block(!app.is_window_focused && app.rail.is_none()).title(" Trees ");
            let body = block.inner(inbox.list);
            frame.render_widget(block, inbox.list);
            draw_table(frame, app, body);
            if let Some(area) = windows.window {
                let screen = frame.area();
                let (cursor_area, other_area) = pane_areas(app, area);
                let shown = chat.filter(|c| !floats(app, is_popup_chat(c), screen));
                let pane = Pane {
                    is_alone: other_area.is_none() && app.other_window().is_some(),
                    ..cursor_pane(app)
                };
                draw_window(frame, app, cursor_area, &pane, shown);
                if let (Some(area), Some(pane)) = (other_area, other_pane(app)) {
                    draw_window(frame, app, area, &pane, other_chat);
                }
            }
            draw_footer(frame, app, footer);
            if let Some(area) = inbox.strip {
                draw_inbox_strip(frame, app, area);
            }
            if let Some(area) = inbox.rail {
                draw_inbox(frame, app, area);
            }
        }
    }
    let popup_chat = chat.filter(|c| floats(app, is_popup_chat(c), frame.area()));
    let floating_tab = !app.is_issues
        && floats(app, false, frame.area())
        && (app.has_keys_on(WindowTab::Diff) || app.has_keys_on(WindowTab::Pr));
    let form = window_form(app, frame.area());
    // A chat on its way floats in a popup unless the window shows it; a
    // window start gives way to any other prompt opened over it.
    let popup_opening = app.opening.as_ref().filter(|o| {
        form.is_none()
            && (o.key.is_none() || app.prompt.is_none() && !has_window(app, frame.area()))
    });
    if popup_chat.is_some()
        || floating_tab
        || app.prompt.is_some() && form.is_none()
        || popup_opening.is_some()
        || app.picker.is_some()
        || app.port_menu.is_some()
        || app.phone.is_some()
        || app.scratch_menu.is_some()
        || app.is_help
    {
        // The inbox beside the list stays bright and live; only what the
        // popup sits over dims.
        let area = stage(app, frame.area());
        frame
            .buffer_mut()
            .set_style(area, Style::new().add_modifier(Modifier::DIM));
    }
    if let Some(chat) = popup_chat {
        draw_chat(frame, app, chat);
    } else if floating_tab {
        draw_floating_tab(frame, app);
    } else if let Some(opening) = popup_opening {
        draw_opening(frame, app, opening);
    } else if let Some(prompt) = &app.prompt {
        draw_prompt(
            frame,
            form.map_or_else(
                || stage(app, frame.area()),
                |area| Block::bordered().inner(area),
            ),
            prompt,
        );
    } else if let Some(picker) = &app.picker {
        draw_picker(frame, app, picker);
    } else if let Some(menu) = &app.port_menu {
        draw_port_menu(frame, menu);
    } else if let Some(phone) = &app.phone {
        draw_phone(frame, phone);
    } else if let Some(menu) = &app.scratch_menu {
        draw_scratch_menu(frame, app, menu);
    } else if app.is_issues {
        // The trees' own dialogs wait until the trees are back on screen.
    } else if let Some(review) = app.reviews.front() {
        draw_review(frame, review, app.reviews.len() - 1);
    } else if let Some(shipping) = &app.shipping {
        draw_shipping(frame, shipping);
    } else if let Some(pending) = &app.pending {
        draw_confirm(frame, pending);
    } else if app.is_help {
        draw_help(frame, &HELP);
    }
}

/// The pool at a glance, for the right end of the bottom bar: counts,
/// actions and how fresh the listing is, closed by the name as a badge, the
/// way lualine closes its statusline. Zero marks and actions are left out.
///
/// `is_compact` keeps only what needs you, what you marked and what is
/// running, for a bar too narrow to show the keys beside the full summary.
fn summary(app: &App, is_compact: bool) -> Vec<Span<'static>> {
    let t = theme();
    let mut spans = vec![Span::raw("  ")];
    if app.is_loaded {
        let pool = app.trees.iter().filter(|t| !t.is_main);
        let (trees, held) = (pool.clone().count(), pool.filter(|t| t.is_held()).count());
        let running = app.trees.iter().filter(|t| !t.processes.is_empty()).count();
        let serving = app.trees.iter().filter(|t| !t.ports().is_empty()).count();
        let agents = app
            .trees
            .iter()
            .map(|t| App::agents_of(t).len())
            .sum::<usize>();
        let waiting = app
            .trees
            .iter()
            .flat_map(App::agents_of)
            .filter(|a| a.status == Some(Status::Waiting))
            .count();
        spans.push(Span::raw(format!("{trees} trees  {held} held")));
        if !is_compact {
            spans.push(Span::styled(format!("  {running} running"), fg(t.warning)));
            spans.push(Span::styled(format!("  {serving} serving"), fg(t.info)));
            let noun = if agents == 1 { "agent" } else { "agents" };
            spans.push(Span::raw(format!("  {agents} {noun}")));
        }
        if waiting > 0 {
            spans.push(Span::styled(
                format!("  {waiting} waiting"),
                fg(t.warning).add_modifier(Modifier::BOLD),
            ));
        }
        if let n @ 1.. = app.needing_you() {
            spans.push(Span::styled(
                format!("  {n} need you"),
                fg(t.danger).add_modifier(Modifier::BOLD),
            ));
        }
        if let n @ 1.. = app.merged() {
            spans.push(Span::styled(
                format!("  {n} merged"),
                fg(t.special).add_modifier(Modifier::BOLD),
            ));
        }
        if let n @ 1.. = app.elsewhere() {
            spans.push(Span::styled(
                format!("  elsewhere: {n} need you"),
                fg(t.warning),
            ));
        }
        if !app.marked.is_empty() {
            spans.push(Span::raw(format!("  {} marked", app.marked.len())));
        }
        let update = if app.replaced.is_some() {
            Some("treetop updated: U restarts".to_string())
        } else if app.is_updating {
            Some("treetop updating...".to_string())
        } else {
            app.newer
                .as_ref()
                .map(|tag| format!("treetop {tag} out: U updates"))
        };
        if let Some(update) = update {
            spans.push(Span::styled(
                format!("  {update}"),
                fg(t.special).add_modifier(Modifier::BOLD),
            ));
        }
        if let n @ 1.. = app.active_jobs() {
            spans.push(Span::styled(
                format!("  {n} action(s) running"),
                fg(t.info).add_modifier(Modifier::BOLD),
            ));
        }
        if !is_compact {
            spans.push(freshness(app));
            if app.is_listing {
                spans.push(Span::styled("  refreshing...", fg(t.info)));
            }
        }
    } else {
        spans.push(Span::raw("reading the pool..."));
    }
    spans.push(Span::raw(" "));
    if let Some(state) = app.assistant_state() {
        let style = match state {
            "asks you" => t.badge.bg(t.warning),
            "answering" => t.badge.bg(t.info),
            _ => t.badge,
        };
        spans.push(Span::styled(format!(" assistant · {state} "), style));
        spans.push(Span::raw(" "));
    }
    let badge = match crate::workspaces::name(&app.checkout) {
        name if name.is_empty() => " treetop ".to_string(),
        name => format!(" treetop · {name} "),
    };
    spans.push(Span::styled(badge, t.badge));
    spans
}

/// How old the pool listing is. Processes and git counts are always within a
/// couple of seconds; this is about leases, which only treehouse knows.
fn freshness(app: &App) -> Span<'static> {
    if app.is_cached {
        return Span::styled("  cached listing", fg(theme().warning));
    }
    let Some(at) = app.listed_at else {
        return Span::raw("");
    };
    let age = at.elapsed().as_secs();
    let age = if age < 60 {
        format!("{age}s")
    } else {
        format!("{}m", age / 60)
    };
    Span::raw(format!("  listed {age} ago"))
}

/// A tree's git state as a Powerlevel10k prompt writes it: `⇡` commits on
/// no remote, `+` staged, `!` unstaged and `?` untracked files, each only
/// when there are some. Nothing for a clean tree.
fn git_spans(work: Work) -> Vec<Span<'static>> {
    behind_spans(work, 0)
}

/// `git_spans` with the commits the tree sits behind its upstream first, as
/// `⇣`, which only the main checkout tracks.
fn behind_spans(work: Work, behind: usize) -> Vec<Span<'static>> {
    let t = theme();
    [
        ("⇣", behind, t.info),
        ("⇡", work.unpushed, t.success),
        ("+", work.staged, t.warning),
        ("!", work.unstaged, t.warning),
        ("?", work.untracked, t.info),
    ]
    .into_iter()
    .filter(|(_, count, _)| *count > 0)
    .enumerate()
    .flat_map(|(at, (symbol, count, color))| {
        let gap = if at == 0 { "" } else { " " };
        [
            Span::raw(gap),
            Span::styled(format!("{symbol}{count}"), fg(color)),
        ]
    })
    .collect()
}

/// The tree's git state, blank when clean or available, and `─`, as
/// Powerlevel10k writes an unknown count, where git could not read a held
/// tree.
fn git_cell(tree: &Tree, behind: usize) -> Cell<'static> {
    match tree.work {
        None if tree.is_held() => Cell::from("─").style(fg(theme().warning)),
        None => Cell::from(""),
        Some(work) => Cell::from(Line::from(behind_spans(work, behind))),
    }
}

/// The tree's job while one is queued, running, just done or failed, else its
/// pool status.
fn status_cell(app: &App, tree: &Tree) -> Cell<'static> {
    let t = theme();
    let bold = Modifier::BOLD;
    let (text, style) = match app.jobs.get(&tree.path) {
        Some(JobState::Queued(_)) => ("queued".to_string(), fg(t.info)),
        Some(JobState::Running(kind)) => (
            format!("{}...", kind.running()),
            fg(t.info).add_modifier(bold),
        ),
        Some(JobState::Done(kind, _)) => {
            (kind.done().to_string(), fg(t.success).add_modifier(bold))
        }
        Some(JobState::Failed(_)) => ("failed".to_string(), fg(t.danger).add_modifier(bold)),
        None if app.is_handing_off(&tree.path) => {
            ("handoff...".to_string(), fg(t.info).add_modifier(bold))
        }
        None => {
            let color = match tree.status.as_str() {
                "leased" => t.success,
                "available" => t.muted,
                "checkout" => t.info,
                _ => t.danger,
            };
            (tree.status.clone(), fg(color))
        }
    };
    Cell::from(text).style(style)
}

/// How an agent's status reads: waiting is the one that needs you, so it is
/// the loud one; a source that cannot tell gets a plain dot.
fn agent_style(status: Option<Status>) -> (&'static str, Style) {
    let t = theme();
    match status {
        Some(Status::Waiting) => ("●", fg(t.warning).add_modifier(Modifier::BOLD)),
        Some(Status::Busy) => ("●", fg(t.success)),
        Some(Status::Idle) => ("○", fg(t.muted)),
        Some(Status::Cold) => ("◌", fg(t.muted).add_modifier(Modifier::ITALIC)),
        None => ("·", Style::new()),
    }
}

/// The tree's leading agent, as `App::agents_of` orders them, and how many
/// more are live, marked by the tree's latest report when it needs you or
/// is done, else by the agent's status; its words are in the detail pane.
/// Assistants never show, and stopped agents only where nothing is live. A
/// held tree with no agent, once agents have been read, says so: it is the
/// one to check on. The main checkout with none is simply blank.
fn agent_cell(app: &App, tree: &Tree) -> Cell<'static> {
    let agents = App::agents_of(tree);
    let Some(first) = agents.first() else {
        return if tree.is_held() && !tree.is_main && app.is_agents_read {
            Cell::from("no agent").style(fg(theme().muted))
        } else {
            Cell::from("")
        };
    };
    let (glyph, style) = match app.says(tree).map(|item| item.state) {
        Some(state @ (State::Blocked | State::Done | State::Failed)) => state_style(state),
        _ => agent_style(first.status),
    };
    let live = agents
        .iter()
        .skip(1)
        .filter(|a| a.status != Some(Status::Cold))
        .count();
    let more = match live {
        0 => String::new(),
        n => format!(" +{n}"),
    };
    let label = first
        .label
        .as_deref()
        .map(|label| format!(" [{label}]"))
        .unwrap_or_default();
    Cell::from(format!("{glyph} {}{label}{more}", first.name)).style(style)
}

/// The tree's keel run in a few words: its playbook and step, then what
/// needs looking at; empty for a tree with none.
fn run_cell(app: &App, tree: &Tree) -> Cell<'static> {
    let t = theme();
    let Some(run) = app.run_of(tree) else {
        return Cell::from("");
    };
    let mut spans = vec![Span::raw(format!("{} {}", run.playbook, run.step()))];
    if run.state != "active" {
        spans.push(Span::styled(format!(" {}", run.state), fg(t.muted)));
    }
    if run.attention > 0 {
        spans.push(Span::styled(
            format!(" !{}", run.attention),
            fg(t.warning).add_modifier(Modifier::BOLD),
        ));
    }
    if run.stale {
        spans.push(Span::styled(" stale", fg(t.danger)));
    }
    Cell::from(Line::from(spans))
}

/// How a PR's stage reads: what needs you is loud, a failure the loudest,
/// and a merge in the colour GitHub gives it.
fn stage_style(stage: Stage) -> Style {
    let t = theme();
    let bold = Modifier::BOLD;
    match stage {
        Stage::CiFailed | Stage::Failed => fg(t.danger).add_modifier(bold),
        Stage::Changes | Stage::Gate => fg(t.warning).add_modifier(bold),
        Stage::Merged => fg(t.special).add_modifier(bold),
        Stage::Approved => fg(t.success),
        Stage::Review | Stage::Checks | Stage::Running => fg(t.info),
        Stage::Draft | Stage::Closed | Stage::Idle => fg(t.muted),
    }
}

/// The tree's PR as its ship plugin labels it, empty for none.
fn pr_cell(tree: &Tree) -> Cell<'static> {
    match &tree.ship {
        Some(ship) => Cell::from(ship.label.clone()).style(stage_style(ship.stage)),
        None => Cell::from(""),
    }
}

/// Every port the tree serves, lowest first, so a frontend and a backend
/// both show; `-` for none. A port shared with the phone carries a `p`
/// after it, and one on the internet a `P`: the key that put it there, so
/// what anyone with the link can open is on the row, not only in the popup
/// that shared it.
fn ports_cell(app: &App, tree: &Tree) -> Cell<'static> {
    Cell::from(Line::from(port_spans(app, tree)))
}

/// The PORTS cell's spans: each port, and the key that shared it where one
/// did.
fn port_spans(app: &App, tree: &Tree) -> Vec<Span<'static>> {
    let t = theme();
    let ports = tree.ports();
    if ports.is_empty() {
        return vec![Span::styled("-", fg(t.muted))];
    }
    let mut spans: Vec<Span> = Vec::new();
    for port in ports {
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(format!(":{port}"), fg(t.info)));
        match app.reach_of(port) {
            Some(true) => {
                spans.push(Span::styled("P", fg(t.accent).add_modifier(Modifier::BOLD)));
            }
            Some(false) => spans.push(Span::styled("p", fg(t.accent))),
            None => {}
        }
    }
    spans
}

fn row(app: &App, tree: &Tree) -> Row<'static> {
    let t = theme();
    let mark = if app.marked.contains(&tree.path) {
        Span::styled("*", fg(t.accent).add_modifier(Modifier::BOLD))
    } else if app.is_here(tree) {
        Span::styled("@", fg(t.info))
    } else {
        Span::raw(" ")
    };
    let branch = match &tree.branch {
        Some(b) => Cell::from(b.clone()),
        None => Cell::from("(detached)").style(fg(t.muted)),
    };
    let name = Line::from(vec![Span::raw(tree.name.clone()), window_marks(app, tree)]);
    let mut cells = vec![
        Cell::from(mark),
        Cell::from(name),
        status_cell(app, tree),
        Cell::from(
            tree.sidecar
                .as_ref()
                .and_then(|s| s.ticket.clone())
                .unwrap_or_default(),
        )
        .style(fg(t.accent)),
        branch,
        agent_cell(app, tree),
    ];
    if !app.runs.is_empty() {
        cells.push(run_cell(app, tree));
    }
    cells.extend([ports_cell(app, tree), git_cell(tree, 0)]);
    if app.is_shipping() {
        cells.push(pr_cell(tree));
    }
    Row::new(cells).style(if app.is_cached {
        Style::new().add_modifier(Modifier::ITALIC)
    } else {
        Style::new()
    })
}

fn draw_table(frame: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let mut titles = vec!["", "#", "STATUS", "TICKET", "BRANCH", "AGENT"];
    if !app.runs.is_empty() {
        titles.push("RUN");
    }
    titles.extend(["PORTS", "GIT"]);
    if app.is_shipping() {
        titles.push("PR");
    }
    let header = Row::new(titles).style(t.header);
    // A blank line sets each heading apart from the trees above it. The
    // cursor never lands on one, so each pushes the rows below it one
    // further down the table.
    let mut rows: Vec<Row> = Vec::new();
    let mut selected = 0;
    for (at, list_row) in app.rows().into_iter().enumerate() {
        if matches!(list_row, ListRow::Heading { .. }) && at > 0 {
            rows.push(Row::new(Vec::<Cell>::new()));
        }
        if at == app.selected {
            selected = rows.len();
        }
        rows.push(match list_row {
            ListRow::Tree(tree) => row(app, tree),
            ListRow::Heading {
                group,
                count,
                waiting,
                is_folded,
            } => heading_row(app, &group, count, waiting, is_folded),
        });
    }
    let mut widths = vec![
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Length(13),
        Constraint::Length(9),
        Constraint::Fill(3),
        Constraint::Fill(2),
    ];
    if !app.runs.is_empty() {
        widths.push(Constraint::Length(18));
    }
    widths.extend([Constraint::Length(17), Constraint::Length(14)]);
    if app.is_shipping() {
        widths.push(Constraint::Length(14));
    }
    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(t.cursor);
    let mut state = TableState::default().with_selected(Some(selected));
    frame.render_stateful_widget(table, area, &mut state);
}

/// In a double, the marks of the windows showing `tree`, left then right,
/// for its row or, for the main checkout, its heading; a tree in both
/// carries both.
fn window_marks(app: &App, tree: &Tree) -> Span<'static> {
    let marks: String = app
        .other_window()
        .map(|(other, _)| {
            let here = app.window_tree().is_some_and(|t| t.path == tree.path);
            let there = other.path == tree.path;
            let on = |side: usize| if side == app.side { here } else { there };
            (0..2)
                .filter(|&side| on(side))
                .map(|side| SIDES[side])
                .collect()
        })
        .unwrap_or_default();
    Span::styled(marks, fg(theme().title))
}

/// A group's heading: folded or open, which group, and how many trees it
/// holds. A base's names its branch in the BRANCH column, wide enough for
/// any name, and the main checkout's branch carries the checkout itself,
/// with its ports, agents and git state in their columns. A folded heading
/// says how many of its trees wait on you instead of naming an agent.
fn heading_row(
    app: &App,
    group: &Group,
    count: usize,
    waiting: usize,
    is_folded: bool,
) -> Row<'static> {
    let t = theme();
    let glyph = if is_folded { "▸" } else { "▾" };
    let main = app
        .trees
        .iter()
        .find(|tree| tree.is_main)
        .filter(|tree| matches!(group, Group::Base(b) if tree.branch.as_ref() == Some(b)));
    let (label, mut name) = match group {
        Group::Base(base) => (
            "base",
            vec![
                Span::styled(
                    if base.is_empty() {
                        "trees".to_string()
                    } else {
                        base.clone()
                    },
                    fg(t.title).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {count}"), fg(t.muted)),
            ],
        ),
        Group::Backburner => (
            "backburner",
            vec![Span::styled(format!("{count} not now"), fg(t.muted))],
        ),
        Group::Free => (
            "free",
            vec![Span::styled(format!("{count} not leased"), fg(t.muted))],
        ),
        Group::Parked => (
            "parked",
            vec![Span::styled(format!("{count} set aside"), fg(t.muted))],
        ),
    };
    if let Some(main) = main {
        name.push(Span::styled("  · main checkout", fg(t.muted)));
        name.push(Span::raw(" "));
        name.push(window_marks(app, main));
    }
    let waits = if is_folded && waiting > 0 {
        Cell::from(format!("● {waiting} waiting on you"))
            .style(fg(t.warning).add_modifier(Modifier::BOLD))
    } else if let Some(main) = main {
        agent_cell(app, main)
    } else {
        Cell::from("")
    };
    let mut cells = vec![
        Cell::from(glyph).style(fg(t.accent)),
        Cell::from(""),
        Cell::from(label).style(fg(t.muted)),
        Cell::from(""),
        Cell::from(Line::from(name)),
        waits,
    ];
    if let Some(main) = main {
        cells.push(ports_cell(app, main));
        cells.push(git_cell(main, app.behind));
    }
    Row::new(cells)
}

/// Where the tree is, who holds it, its agents, ports and processes.
fn detail_lines(app: &App, tree: &Tree) -> Vec<Line<'static>> {
    let t = theme();
    let since = tree
        .leased_at
        .as_deref()
        .map(|t| t.get(..16).unwrap_or(t).replace('T', " "));
    let procs = if tree.processes.is_empty() {
        "none".to_string()
    } else {
        tree.processes
            .iter()
            .map(|p| format!("{} ({})", p.name, p.pid))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut ports: Vec<(u16, &str, u32)> = tree
        .processes
        .iter()
        .flat_map(|p| {
            p.ports
                .iter()
                .map(move |&port| (port, p.name.as_str(), p.pid))
        })
        .collect();
    ports.sort_unstable();
    ports.dedup_by_key(|(port, _, _)| *port);
    let ports = if ports.is_empty() {
        "none".to_string()
    } else {
        ports
            .iter()
            .map(|(port, name, pid)| format!("localhost:{port} {name} ({pid})"))
            .collect::<Vec<_>>()
            .join("   ")
    };
    let holder = tree
        .holder
        .as_ref()
        .map(|h| format!("  held by {h}"))
        .unwrap_or_default();
    // Where a held tree was cut from, which its diff and PR go against.
    let base = app
        .base_of(tree)
        .filter(|_| tree.is_held() && !tree.is_main)
        .map(|b| format!("  from {b}"))
        .unwrap_or_default();
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                tree.path.display().to_string(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                since.map(|s| format!("  leased {s}")).unwrap_or_default(),
                fg(t.muted),
            ),
            Span::styled(holder, fg(t.muted)),
            Span::styled(base, fg(t.muted)),
        ]),
        agents_line(app, tree),
        Line::from(vec![
            Span::styled("ports     ", fg(t.muted)),
            Span::raw(ports),
        ]),
        Line::from(vec![
            Span::styled("processes ", fg(t.muted)),
            Span::raw(procs),
        ]),
    ];
    if let Some(run) = app.run_of(tree) {
        lines.push(run_line(run));
    }
    if app.is_shipping() && tree.is_held() && !tree.is_main {
        lines.push(pr_line(app, tree));
    }
    lines
}

/// The tree's keel run: its playbook, the step it is on, its state and
/// what needs looking at, then its goal.
fn run_line(run: &crate::keel::Run) -> Line<'static> {
    let t = theme();
    let mut spans = vec![
        Span::styled("run       ", fg(t.muted)),
        Span::styled(
            run.playbook.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  step {}", run.step())),
    ];
    if let Some(title) = run.step_title() {
        spans.push(Span::raw(format!(" {title}")));
    }
    spans.push(Span::styled(format!("  {}", run.state), fg(t.muted)));
    if run.attention > 0 {
        spans.push(Span::styled(
            format!("  {} need attention", run.attention),
            fg(t.warning).add_modifier(Modifier::BOLD),
        ));
    }
    if run.stale {
        spans.push(Span::styled(
            "  stale: no process in the tree",
            fg(t.danger),
        ));
    }
    if !run.goal.is_empty() {
        spans.push(Span::styled(format!("  ·  {}", run.goal), fg(t.muted)));
    }
    Line::from(spans)
}

/// The tree's PR: its label, its link and what its plugin says about it;
/// why nobody could say, when nobody could.
fn pr_line(app: &App, tree: &Tree) -> Line<'static> {
    let t = theme();
    let mut spans = vec![Span::styled("pr        ", fg(t.muted))];
    match (&tree.ship, &app.ship_error) {
        (Some(ship), _) => {
            spans.push(Span::styled(ship.label.clone(), stage_style(ship.stage)));
            if let Some(url) = &ship.url {
                spans.push(Span::raw(format!("  {url}")));
            }
            for line in &ship.detail {
                spans.push(Span::styled(format!("  ·  {line}"), fg(t.muted)));
            }
        }
        (None, Some(why)) => spans.push(Span::styled(format!("unknown: {why}"), fg(t.muted))),
        (None, None) => spans.push(Span::styled("none", fg(t.muted))),
    }
    Line::from(spans)
}

/// Each agent with its status, where it was seen and its pid.
/// Every agent in the tree; in the main checkout only those its heading
/// goes by, since the assistant and old sessions run there in place too.
fn agents_line(app: &App, tree: &Tree) -> Line<'static> {
    let t = theme();
    let mut spans = vec![Span::styled("agents    ", fg(t.muted))];
    let agents: Vec<&Agent> = if tree.is_main {
        App::agents_of(tree)
    } else {
        tree.agents.iter().collect()
    };
    if agents.is_empty() {
        let text = if app.is_agents_read {
            "none"
        } else {
            "reading..."
        };
        spans.push(Span::raw(text));
        return Line::from(spans);
    }
    for (i, agent) in agents.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        spans.extend(agent_spans(agent));
    }
    Line::from(spans)
}

fn agent_spans(agent: &Agent) -> Vec<Span<'static>> {
    let (glyph, style) = agent_style(agent.status);
    let mut about = vec![agent.source.label().to_string()];
    if let Some(pid) = agent.pid {
        about.push(format!("pid {pid}"));
    }
    if let Some(status) = agent.status {
        about.insert(0, status.label().to_string());
    }
    if agent.is_background {
        about.push("background".into());
    }
    if let Some(label) = &agent.label {
        about.insert(0, label.clone());
    }
    vec![
        Span::styled(format!("{glyph} {}", agent.name), style),
        Span::styled(format!(" ({})", about.join(", ")), fg(theme().muted)),
    ]
}

/// Keys as badges on a bar, as lualine draws its sections.
fn key_bar(keys: &[(&str, &str)]) -> Vec<Span<'static>> {
    let t = theme();
    keys.iter()
        .flat_map(|(k, label)| {
            [
                Span::styled(format!(" {k} "), t.badge),
                Span::raw(format!(" {label} ")),
            ]
        })
        .collect()
}

/// The keys that fit in `width` columns, dropping whole keys from the end
/// rather than cutting one in half.
fn fitting_keys(keys: &[(&str, &str)], width: u16) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut used = 0;
    for key in keys.chunks(1) {
        let pair = key_bar(key);
        let pair_width: usize = pair.iter().map(Span::width).sum();
        if used + pair_width > usize::from(width) {
            break;
        }
        used += pair_width;
        spans.extend(pair);
    }
    spans
}

/// What the left of the bottom bar says: the filter being typed, the error or
/// message there is, else the keys.
fn footer_left(app: &App, width: u16) -> Line<'static> {
    let t = theme();
    if app.is_filtering {
        return Line::raw(format!(
            " Filter: {}_   Enter keeps it, Esc clears it",
            app.filter
        ));
    }
    if let Some(err) = &app.error {
        return Line::styled(format!(" {err}"), fg(t.danger));
    }
    if let Some(message) = &app.message {
        return Line::styled(format!(" {message}"), fg(t.warning));
    }
    // What narrows the list is placed first, so the keys give way to it
    // rather than push it off the bar.
    let mut narrowing = Vec::new();
    if !app.filter.is_empty() {
        narrowing.push(Span::styled(
            format!(" filter: {}", app.filter),
            fg(t.accent),
        ));
    }
    let used = u16::try_from(narrowing.iter().map(Span::width).sum::<usize>()).unwrap_or(width);
    let mut spans = fitting_keys(&KEYS, width.saturating_sub(used));
    spans.extend(narrowing);
    Line::from(spans)
}

/// The bottom bar: keys or a message on the left, the summary on the right.
fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    // The keys keep at least KEY_ROOM columns; a full summary that would
    // squeeze them gives way to the compact one.
    const KEY_ROOM: u16 = 40;
    let full = Line::from(summary(app, false));
    let right = if full.width() > usize::from(area.width.saturating_sub(KEY_ROOM)) {
        Line::from(summary(app, true))
    } else {
        full
    };
    let right_width = u16::try_from(right.width())
        .unwrap_or(u16::MAX)
        .min(area.width);
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(right_width)]).areas(area);
    frame.render_widget(
        Paragraph::new(footer_left(app, left_area.width)).style(t.bar),
        left_area,
    );
    frame.render_widget(Paragraph::new(right).style(t.bar), right_area);
}

/// Rows `lines` take wrapped at `width`, near enough to size a pane by:
/// wrapping breaks at words, so it can take a row more than this says.
fn wrapped_height(lines: &[Line], width: u16) -> usize {
    let width = usize::from(width.max(1));
    lines
        .iter()
        .map(|line| line.width().div_ceil(width).max(1))
        .sum()
}

/// How a report's state reads: what needs you is the loud one.
fn state_style(state: State) -> (&'static str, Style) {
    let t = theme();
    match state {
        State::Blocked => ("●", fg(t.warning).add_modifier(Modifier::BOLD)),
        State::Done => ("✓", fg(t.success)),
        State::Working => ("·", fg(t.info)),
        State::Stopped => ("■", fg(t.muted)),
        State::Failed => ("✗", fg(t.danger)),
    }
}

/// Columns the inbox takes beside the list, its border included.
const INBOX_WIDTH: u16 = 38;

/// The narrowest terminal the inbox sits beside the list in full: the
/// list's fixed columns take about a hundred, so this leaves BRANCH and
/// AGENT room to read. Below it, a strip of counts stands in, and the
/// inbox opens over the list while it has the keys.
const INBOX_FULL_FROM: u16 = 180;

/// Columns the strip of counts takes, its border included.
const STRIP_WIDTH: u16 = 5;

/// The smallest the table's window gets, its border included: its header
/// and a few trees.
const TABLE_MIN: u16 = 7;

/// The share of the screen above the bar, in percent, the table's window
/// takes at most, so the window under it keeps room for a chat.
const TABLE_SHARE: u16 = 45;

/// The smallest window under the table worth drawing, its border and tab
/// bar included; below it the table takes the whole screen.
const WINDOW_MIN: u16 = 5;

/// Where the Trees screen's windows go in `main`, the screen above the bar.
struct Windows {
    /// The table and the inbox beside it.
    top: Rect,
    /// The window under them, showing the tree under the cursor.
    window: Option<Rect>,
}

/// The table's window is as tall as its rows, or the inbox's entries
/// beside it where there are more, between TABLE_MIN and TABLE_SHARE of
/// `main`; the window under it takes the rest.
fn windows(app: &App, main: Rect) -> Windows {
    let rows = app.rows();
    // Each heading after the first has a blank line above it.
    let blanks = rows
        .iter()
        .skip(1)
        .filter(|row| matches!(row, ListRow::Heading { .. }))
        .count();
    let table = u16::try_from(rows.len() + blanks)
        .unwrap_or(u16::MAX)
        .saturating_add(3);
    // The inbox beside the table in full wants room for its entries too:
    // its title, its lines and its keys, inside its border.
    let inbox = if main.width >= INBOX_FULL_FROM && !app.inbox.is_hidden {
        let lines = inbox_lines(app, Rect::new(0, 0, INBOX_WIDTH - 4, u16::MAX)).len();
        u16::try_from(lines).unwrap_or(u16::MAX).saturating_add(5)
    } else {
        0
    };
    let wanted = table.max(inbox);
    let most = (main.height * TABLE_SHARE / 100).max(TABLE_MIN);
    let height = wanted.clamp(TABLE_MIN, most);
    if main.height < height + WINDOW_MIN {
        return Windows {
            top: main,
            window: None,
        };
    }
    let [top, window] =
        Layout::vertical([Constraint::Length(height), Constraint::Fill(1)]).areas(main);
    Windows {
        top,
        window: Some(window),
    }
}

/// A window's border: the title colour on the one that has the keys, as
/// Neovim marks the window the cursor is in.
fn window_block(has_keys: bool) -> Block<'static> {
    let t = theme();
    let color = if has_keys { t.title } else { t.border };
    Block::bordered()
        .border_style(fg(color))
        .title_style(Style::new().add_modifier(Modifier::BOLD))
}

/// A chat that floats in a popup rather than the window under the table:
/// the assistant's and the scratch chats, which belong to no tree.
pub fn is_popup(agent: &str, tree: &str) -> bool {
    assistant::is_assistant(agent) || tree == SCRATCH
}

pub fn is_popup_chat(chat: &Chat) -> bool {
    is_popup(&chat.agent, &chat.tree)
}

/// A chat drawn in a popup: the assistant's and the scratch ones always,
/// and a tree's while it has the keys where the window under the table is
/// not on `screen`, over the Issues screen or on a terminal too short for
/// the window. Without the keys there it is not drawn at all, so it never
/// covers what has them.
pub fn floats(app: &App, is_popup: bool, screen: Rect) -> bool {
    is_popup || app.is_window_focused && (app.is_issues || window_chat_area(app, screen).is_none())
}

/// Inside the window under the table: its tab bar, then where its chat or
/// info goes.
fn window_areas(area: Rect) -> [Rect; 2] {
    let inner = Block::bordered().inner(area);
    Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner)
}

/// Where the chat in the window under the table goes on `screen`, for
/// sizing its terminal and placing its mouse events.
pub fn window_chat_area(app: &App, screen: Rect) -> Option<Rect> {
    let [main, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(screen);
    let window = windows(app, main).window?;
    Some(window_areas(pane_areas(app, window).0)[1])
}

/// The two windows' marks, left and right, in their titles and on their
/// trees' rows.
const SIDES: [&str; 2] = ["◧", "◨"];

/// The narrowest window area two windows share side by side; below it a
/// double shows the window the cursor drives alone.
const DOUBLE_FROM: u16 = 140;

/// Where the window the cursor drives goes in `window`, and the second
/// window of a double beside it: halves on its side, or the whole for the
/// cursor's window alone.
fn pane_areas(app: &App, window: Rect) -> (Rect, Option<Rect>) {
    if app.other_window().is_none() || window.width < DOUBLE_FROM {
        return (window, None);
    }
    // Equal halves, an odd column left blank at the right, so one chat size
    // fits either window.
    let half = window.width / 2;
    let halves: [Rect; 2] = [
        Rect {
            width: half,
            ..window
        },
        Rect {
            x: window.x + half,
            width: half,
            ..window
        },
    ];
    (halves[app.side], Some(halves[1 - app.side]))
}

/// Where a new agent's compose box and its start go: the window the cursor
/// drives, while one is on screen. None draws them in a popup, as before:
/// the assistant's, a scratch chat's, and over the Issues screen.
fn window_form(app: &App, screen: Rect) -> Option<Rect> {
    if app.is_issues {
        return None;
    }
    let [main, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(screen);
    let (cursor, other) = pane_areas(app, windows(app, main).window?);
    if app.prompt.as_ref().is_some_and(Prompt::starts_agent) {
        return Some(cursor);
    }
    // A start stays in the window it was asked from, wherever the keys
    // have gone since; on a terminal too narrow for both it shows only
    // while that window is the one on screen.
    let start = app.window_start().filter(|_| app.prompt.is_none())?;
    match start.side.filter(|_| app.other_window().is_some()) {
        Some(side) if side != app.side => other,
        _ => Some(cursor),
    }
}

/// The window under the table is on screen, for a tree's start to show in
/// rather than a popup.
fn has_window(app: &App, screen: Rect) -> bool {
    !app.is_issues && window_chat_area(app, screen).is_some()
}

/// The steps a start goes through, in order: the name its log gives each,
/// and what the window says for it.
const START_STEPS: [(&str, &str); 6] = [
    ("lease", "lease a tree"),
    ("check", "check it is a clean worktree"),
    ("branch", "name its branch"),
    ("record", "write treetop.json"),
    ("agent", "start the agent"),
    ("attach", "find its chat"),
];

/// A start under way in the window: each step ticked as it ends, a step
/// it went past marked skipped, the one under way, or the one it failed
/// at with why.
fn start_lines(app: &App, opening: &Opening, key: &std::path::Path) -> Vec<Line<'static>> {
    let t = theme();
    let done = app.steps.get(key).map(Vec::as_slice).unwrap_or_default();
    let last = START_STEPS
        .iter()
        .rposition(|(step, _)| done.contains(step));
    let mut lines = vec![
        Line::styled(
            format!("Starting {}", opening.agent),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
    ];
    let mut is_next = true;
    for (at, (step, label)) in START_STEPS.iter().enumerate() {
        // A start in a tree already held leases, branches and records
        // nothing.
        if !opening.leases && !matches!(*step, "agent" | "attach") {
            continue;
        }
        let (glyph, style, note) = if done.contains(step) {
            ("✓", fg(t.success), String::new())
        } else if last.is_some_and(|last| at < last) {
            ("-", fg(t.muted), "  not needed here".to_string())
        } else if is_next {
            is_next = false;
            match &opening.failed {
                Some(why) => ("✗", fg(t.danger), format!("  {why}")),
                None => (
                    "◌",
                    fg(t.warning).add_modifier(Modifier::BOLD),
                    String::new(),
                ),
            }
        } else {
            ("·", fg(t.muted), String::new())
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{glyph} "), style),
            Span::styled(*label, style),
            Span::styled(note, fg(t.danger)),
        ]));
    }
    if is_next && opening.failed.is_none() {
        lines.push(Line::styled("◌ waiting for its chat", fg(t.warning)));
    }
    lines.push(Line::raw(""));
    let keys = if opening.failed.is_some() {
        "Esc closes this"
    } else {
        "Esc hides this; the start carries on"
    };
    lines.push(Line::styled(keys, fg(t.muted)));
    lines
}

/// What a window draws: the tree it shows and its tabs, whether it has the
/// keys, its mark in a double, and whether the cursor drives it.
struct Pane<'a> {
    tree: Option<&'a Tree>,
    tab: WindowTab,
    tabs: Vec<WindowTab>,
    has_keys: bool,
    side: Option<usize>,
    is_cursor: bool,
    /// One of a double shown alone, the terminal too narrow for both.
    is_alone: bool,
}

/// The window the cursor drives.
fn cursor_pane(app: &App) -> Pane<'_> {
    let is_double = app.other_window().is_some();
    Pane {
        tree: app.window_tree(),
        tab: app.shown_tab(),
        tabs: app.window_tabs(),
        has_keys: app.is_window_focused,
        side: is_double.then_some(app.side),
        is_cursor: true,
        is_alone: false,
    }
}

/// The second window of a double, keeping its tree while the cursor
/// drives the other.
fn other_pane(app: &App) -> Option<Pane<'_>> {
    let (tree, tab) = app.other_window()?;
    Some(Pane {
        tree: Some(tree),
        tab,
        tabs: app.tabs_of(tree),
        has_keys: false,
        side: Some(1 - app.side),
        is_cursor: false,
        is_alone: false,
    })
}

/// A tab's name in the window's bar, with what the PR column or the run
/// says of it where there is something.
fn tab_label(app: &App, tree: Option<&Tree>, tab: WindowTab) -> String {
    match tab {
        WindowTab::Chat => "chat".into(),
        WindowTab::Diff => "diff".into(),
        WindowTab::Pr => match tree.and_then(|t| t.ship.as_ref()) {
            Some(ship) => format!("PR {}", ship.label),
            None => "PR".into(),
        },
        WindowTab::Run => match tree.and_then(|t| app.run_of(t)) {
            Some(run) => format!("run {}", run.step()),
            None => "run".into(),
        },
        WindowTab::Shell => "shell".into(),
        WindowTab::Info => "info".into(),
    }
}

/// The window's tabs, the one shown picked out, each with the columns it
/// takes in the bar, the same for drawing them as for a click.
fn tab_spans(app: &App, pane: &Pane) -> Vec<(WindowTab, Span<'static>)> {
    let t = theme();
    pane.tabs
        .iter()
        .map(|&tab| {
            let style = if tab == pane.tab { t.header } else { t.bar };
            (
                tab,
                Span::styled(format!(" {} ", tab_label(app, pane.tree, tab)), style),
            )
        })
        .collect()
}

/// The steps of the keel run in `tree`, each with its state, the one under
/// way picked out.
fn run_lines(app: &App, tree: &Tree) -> Vec<Line<'static>> {
    let t = theme();
    let Some(run) = app.run_of(tree) else {
        return vec![Line::styled("No keel run holds this tree.", fg(t.muted))];
    };
    let mut lines = vec![
        Line::styled(
            format!("keel run · {} · {}", run.playbook, run.goal),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
    ];
    for (index, step) in run.steps.iter().enumerate() {
        let (glyph, style) = match step.state.as_str() {
            "done" => ("✓", fg(t.success)),
            "doing" => ("◌", fg(t.warning).add_modifier(Modifier::BOLD)),
            "skipped" => ("-", fg(t.muted)),
            _ => ("·", Style::new()),
        };
        let is_current = run.current == Some(index + 1);
        let row = Style::new().patch(if is_current { t.cursor } else { Style::new() });
        lines.push(
            Line::from(vec![
                Span::styled(format!("{:>2} ", index + 1), fg(t.muted)),
                Span::styled(format!("{glyph} "), style),
                Span::styled(step.title.clone(), style),
            ])
            .style(row),
        );
    }
    lines
}

/// How full the window agent's context is and what its session has spent,
/// for the right of its tab bar, where the chat popup's sidebar has them in
/// full.
fn context_note(agent: Option<&Agent>) -> Vec<Span<'static>> {
    let t = theme();
    let Some(agent) = agent else {
        return Vec::new();
    };
    let mut spans = Vec::new();
    if let Some(context) = &agent.context {
        spans.push(Span::styled(
            format!(
                "context {}% of {}",
                percent(context),
                tokens(context.window)
            ),
            context_style(context),
        ));
    }
    if let Some(spend) = &agent.spend {
        let total = spend.input + spend.cache_read + spend.cache_write + spend.output;
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", fg(t.muted)));
        }
        spans.push(Span::styled(
            format!("{} tokens spent", tokens(total)),
            fg(t.muted),
        ));
    }
    if !spans.is_empty() {
        spans.push(Span::raw(" "));
    }
    spans
}

/// The window under the table: the tree under the cursor, named in its
/// border, its tabs in a bar on the first row, and under them its agent's
/// chat, live, or its info. `chat` is the chat open in the window, which
/// shows only while it is the window agent's.
fn draw_window(frame: &mut Frame, app: &App, area: Rect, pane: &Pane, chat: Option<&Chat>) {
    let t = theme();
    // The cursor's window gives way to a new agent's compose box, drawn
    // over it, and then to its start until its chat opens.
    if window_form(app, frame.area()) == Some(area) {
        let mark = pane
            .side
            .map(|side| format!("{} ", SIDES[side]))
            .unwrap_or_default();
        let block = window_block(true).title(format!(" {mark}New agent "));
        let body = Block::new()
            .padding(Padding::horizontal(1))
            .inner(block.inner(area));
        frame.render_widget(block, area);
        if app.prompt.is_none()
            && let Some(opening) = app.window_start()
            && let Some(key) = opening.key.as_deref()
        {
            frame.render_widget(Paragraph::new(start_lines(app, opening, key)), body);
        }
        return;
    }
    let tree = pane.tree;
    let mark = pane
        .side
        .map(|side| format!("{} ", SIDES[side]))
        .unwrap_or_default();
    // Alone on a terminal too narrow for both, a window says it has a twin.
    let alone = if pane.is_alone {
        format!("{} of 2 · ", pane.side.unwrap_or(0) + 1)
    } else {
        String::new()
    };
    let title = tree.map(|tree| {
        let branch = tree.branch.as_deref().unwrap_or("(detached)");
        if tree.is_main {
            format!(" {mark}{alone}main checkout · {branch} ")
        } else {
            format!(" {mark}{alone}Tree {} · {branch} ", tree.name)
        }
    });
    let block = window_block(pane.has_keys).title(title.unwrap_or_default());
    frame.render_widget(block, area);
    let [bar, body] = window_areas(area);
    let Some(tree) = tree else {
        let text = match app.current_heading() {
            Some(_) => "a group's heading: Enter folds it or opens it again",
            None if app.is_loaded => "no trees match",
            None => "",
        };
        let body = Block::new().padding(Padding::horizontal(1)).inner(body);
        frame.render_widget(Paragraph::new(text).style(fg(t.muted)), body);
        return;
    };
    let agent = App::chat_agent(tree);
    let tabs: Vec<Span> = tab_spans(app, pane)
        .into_iter()
        .map(|(_, span)| span)
        .collect();
    frame.render_widget(Paragraph::new(Line::from(tabs)).style(t.bar), bar);
    let note = Line::from(context_note(agent)).right_aligned();
    if note.width() + 14 <= usize::from(bar.width) {
        frame.render_widget(Paragraph::new(note), bar);
    }
    let shown_tab = pane.tab;
    if shown_tab == WindowTab::Shell {
        match chat.filter(|c| c.tree == tree.name && c.agent == crate::app::SHELL) {
            Some(chat) if chat.is_ready() => {
                let cursor = draw_screen(chat.screen(), frame.buffer_mut(), body);
                if let Some(cursor) = cursor.filter(|_| pane.has_keys) {
                    frame.set_cursor_position(cursor);
                }
            }
            _ => draw_note(frame, body, "opening a shell in the tree..."),
        }
        return;
    }
    if matches!(shown_tab, WindowTab::Diff | WindowTab::Pr) {
        draw_review_tab(frame, app, tree, shown_tab, pane.is_cursor, body);
        return;
    }
    if shown_tab == WindowTab::Chat
        && let Some(agent) = agent
    {
        let shown = chat.filter(|c| c.tree == tree.name && c.agent != crate::app::SHELL);
        match shown {
            Some(chat) if !chat.is_ready() => draw_note(frame, body, &opening_note(&chat.agent)),
            Some(chat) => {
                let cursor = draw_screen(chat.screen(), frame.buffer_mut(), body);
                if let Some(cursor) = cursor.filter(|_| pane.has_keys) {
                    frame.set_cursor_position(cursor);
                }
            }
            _ if agent.status == Some(Status::Cold) => draw_note(
                frame,
                body,
                &format!("{} is not running; Enter starts it again", agent.name),
            ),
            _ => draw_note(frame, body, &opening_note(&agent.name)),
        }
        return;
    }
    // The info sits a column in from the border, as the inbox's entries
    // do; a chat takes the whole body, as its own screen is drawn.
    let body = Block::new().padding(Padding::horizontal(1)).inner(body);
    let lines = if shown_tab == WindowTab::Run {
        run_lines(app, tree)
    } else {
        detail_text(app, tree, body)
    };
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
}

/// The diff or the PR of `tree`, as `tab` shows it, in `area`: where the
/// cursor drives the window, as scrolled; in the other window of a double,
/// from its top, as last read.
fn draw_review_tab(
    frame: &mut Frame,
    app: &App,
    tree: &Tree,
    tab: WindowTab,
    is_cursor: bool,
    area: Rect,
) {
    if tab == WindowTab::Diff {
        let view = app
            .view
            .as_ref()
            .filter(|v| is_cursor && v.path == tree.path);
        match view {
            Some(view) => draw_diff_view(frame, app, view, area),
            None if !is_cursor && app.diffs.contains_key(&tree.path) => {
                let still = DiffView {
                    path: tree.path.clone(),
                    scroll: 0,
                    hscroll: 0,
                };
                draw_diff_view(frame, app, &still, area);
            }
            None if !is_cursor => {
                draw_note(frame, area, "the diff reads once this window has the keys")
            }
            None => draw_note(frame, area, "reading the diff..."),
        }
    } else {
        let popup = app
            .pr_popup
            .as_ref()
            .filter(|p| is_cursor && p.path == tree.path);
        match popup {
            Some(popup) => draw_pr_view(frame, app, popup, area),
            None if !is_cursor && app.prs.contains_key(&tree.path) => {
                draw_pr_view(frame, app, &PrPopup::new(tree.path.clone()), area);
            }
            None if !is_cursor => {
                draw_note(frame, area, "the PR reads once this window has the keys")
            }
            None => draw_note(frame, area, "reading the PR..."),
        }
    }
}

/// The diff or PR tab with the keys, in a popup where the window under the
/// table is not on screen, so what has the keys is always seen.
fn draw_floating_tab(frame: &mut Frame, app: &App) {
    let Some(tree) = app.window_tree() else {
        return;
    };
    let area = popup::area(stage(app, frame.area()));
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_style(fg(theme().accent))
        .style(fg(theme().text))
        .title(format!(" Tree {} ", tree.name));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    draw_review_tab(frame, app, tree, app.shown_tab(), true, inner);
}

/// Where the list and the inbox go in `main`, the screen above the bar.
struct InboxLayout {
    list: Rect,
    /// The inbox in full, beside the list or over it.
    rail: Option<Rect>,
    /// The strip of counts standing in for it on a narrow terminal.
    strip: Option<Rect>,
}

fn inbox_layout(app: &App, main: Rect) -> InboxLayout {
    let is_wide = main.width >= INBOX_FULL_FROM;
    let beside = match (app.inbox.is_hidden, is_wide) {
        (true, _) => 0,
        (false, true) => INBOX_WIDTH,
        (false, false) => STRIP_WIDTH,
    };
    let [list, side] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(beside)]).areas(main);
    let side = (beside > 0).then_some(side);
    if beside == INBOX_WIDTH {
        return InboxLayout {
            list,
            rail: side,
            strip: None,
        };
    }
    // Narrow or hidden, it opens over the list's right side while it has
    // the keys.
    let over = app.rail.map(|_| {
        let width = INBOX_WIDTH.min(main.width);
        Rect {
            x: main.right() - width,
            width,
            ..main
        }
    });
    InboxLayout {
        list,
        rail: over,
        strip: side,
    }
}

/// Where a popup goes on the list's screen `screen`: all of it but the
/// inbox beside the list, so the inbox stays in view and keeps updating
/// beside a chat, a diff or the compose box. Where the inbox is hidden, or
/// on the Issues screen, the whole screen.
pub fn stage(app: &App, screen: Rect) -> Rect {
    if app.is_issues {
        return screen;
    }
    let [main, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(screen);
    Rect {
        width: inbox_layout(app, windows(app, main).top).list.width,
        ..screen
    }
}

/// The inbox beside the list, as a column or a strip, is under `mouse`.
pub fn is_on_inbox(app: &App, screen: Rect, mouse: ratatui::crossterm::event::MouseEvent) -> bool {
    if app.is_issues {
        return false;
    }
    let [main, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(screen);
    let layout = inbox_layout(app, windows(app, main).top);
    let at = Position::new(mouse.column, mouse.row);
    layout
        .rail
        .or(layout.strip)
        .is_some_and(|area| area.contains(at))
}

/// What a click on the list's screen does: on an inbox entry, opens it; on
/// the strip, gives the inbox the keys. None for anywhere else.
pub fn click(
    app: &mut App,
    screen: Rect,
    mouse: ratatui::crossterm::event::MouseEvent,
) -> Option<Outcome> {
    use ratatui::crossterm::event::{MouseButton, MouseEventKind};
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) || !app.wants_mouse() {
        return None;
    }
    let [main, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(screen);
    let windows = windows(app, main);
    let layout = inbox_layout(app, windows.top);
    let at = Position::new(mouse.column, mouse.row);
    // A tab in the window's bar shows that tab.
    if let Some(window) = windows.window {
        let (cursor_area, other_area) = pane_areas(app, window);
        // A click on the other window of a double gives it the keys.
        if other_area.is_some_and(|area| area.contains(at)) {
            app.focus_side(1 - app.side);
            return Some(Outcome::Continue);
        }
        let [bar, _] = window_areas(cursor_area);
        if bar.contains(at) {
            let mut x = bar.x;
            for (tab, span) in tab_spans(app, &cursor_pane(app)) {
                let width = u16::try_from(span.width()).unwrap_or(u16::MAX);
                if (x..x.saturating_add(width)).contains(&at.x) {
                    app.tab = tab;
                    return Some(Outcome::Continue);
                }
                x = x.saturating_add(width);
            }
        }
    }
    if let Some(rail) = layout.rail.filter(|r| r.contains(at)) {
        let [_, body, _] = inbox_areas(app, rail);
        let row = usize::from(at.y.checked_sub(body.y)?);
        let index = inbox_lines(app, body).get(row)?.1?;
        return Some(app.open_entry(index));
    }
    if layout.strip.is_some_and(|s| s.contains(at)) {
        app.rail = Some(0);
        return Some(Outcome::Continue);
    }
    // A click on the list while the inbox has the keys gives them back.
    app.rail.take().map(|_| Outcome::Continue)
}

/// The inbox's title, entries and keys, inside its border, the same for
/// drawing it as for finding the entry a click lands on.
fn inbox_areas(app: &App, area: Rect) -> [Rect; 3] {
    Layout::vertical([
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(inbox_block(app).inner(area))
}

fn inbox_block(app: &App) -> Block<'static> {
    let t = theme();
    let border = if app.rail.is_some() {
        t.accent
    } else {
        t.border
    };
    Block::bordered()
        .border_style(fg(border))
        .padding(Padding::horizontal(1))
}

/// The mark an entry's kind goes by, as the AGENT column draws its
/// agent's.
fn entry_mark(kind: Kind) -> (&'static str, Style) {
    let t = theme();
    match kind {
        Kind::NeedsYou => ("●", fg(t.warning).add_modifier(Modifier::BOLD)),
        Kind::Failed | Kind::Died => ("✗", fg(t.danger)),
        Kind::Done => ("✓", fg(t.success)),
        Kind::Merged => ("✓", fg(t.special)),
    }
}

/// `text` cut to `width` columns, with an ellipsis where it was cut.
fn fit(text: &str, width: usize) -> String {
    if Line::raw(text).width() <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = Span::raw(c.encode_utf8(&mut [0; 4]).to_string()).width();
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// An entry's two lines: its mark, workspace, tree and title with its age
/// at the right, then what it is about.
fn entry_lines(entry: &Entry, width: usize, style: Style) -> [Line<'static>; 2] {
    let t = theme();
    let (mark, mark_style) = entry_mark(entry.kind);
    let age = entry.at.map(reports::age).unwrap_or_default();
    let mut head = vec![Span::styled(mark, mark_style), Span::raw(" ")];
    if let Some(away) = &entry.away {
        head.push(Span::styled(format!("{} ", away.name), fg(t.info)));
    }
    head.push(Span::styled(
        entry.tree.clone(),
        Style::new().add_modifier(Modifier::BOLD),
    ));
    let used = Line::from(head.clone()).width() + 1;
    let room = width.saturating_sub(used + age.len() + 1);
    let title = fit(entry.title.as_deref().unwrap_or(""), room);
    head.push(Span::raw(format!(" {title}")));
    let pad = width.saturating_sub(Line::from(head.clone()).width() + age.len());
    head.push(Span::raw(" ".repeat(pad)));
    head.push(Span::styled(age, fg(t.muted)));
    // Padded to the full width, as the first line is, so the cursor covers
    // the whole entry.
    let what = format!("  {}", fit(&entry.what, width.saturating_sub(2)));
    let pad = width.saturating_sub(Line::raw(what.as_str()).width());
    let what = Line::styled(format!("{what}{}", " ".repeat(pad)), fg(t.muted));
    [Line::from(head).style(style), what.patch_style(style)]
}

/// The inbox's lines for `inner`, each with the entry it belongs to:
/// headings, then each entry's two lines, scrolled so the cursor's entry
/// shows.
fn inbox_lines(app: &App, inner: Rect) -> Vec<(Line<'static>, Option<usize>)> {
    let t = theme();
    let width = usize::from(inner.width);
    let shown = inbox::shown(app);
    if shown.is_empty() {
        return vec![(Line::styled("Nothing needs you.", fg(t.muted)), None)];
    }
    let mut lines = Vec::new();
    let mut heading = None;
    for (index, (entry, is_seen)) in shown.iter().enumerate() {
        let this = if *is_seen {
            "SEEN"
        } else {
            entry.kind.heading()
        };
        if heading != Some(this) {
            if heading.is_some() {
                lines.push((Line::raw(""), None));
            }
            lines.push((
                Line::styled(this, fg(t.muted).add_modifier(Modifier::BOLD)),
                None,
            ));
            heading = Some(this);
        }
        let mut style = Style::new();
        if *is_seen {
            style = style.add_modifier(Modifier::DIM);
        }
        if app.rail == Some(index) {
            style = t.cursor;
        }
        for line in entry_lines(entry, width, style) {
            lines.push((line, Some(index)));
        }
    }
    let height = usize::from(inner.height);
    let last = app
        .rail
        .and_then(|at| lines.iter().rposition(|(_, i)| *i == Some(at)))
        .unwrap_or(0);
    let skip = (last + 1).saturating_sub(height);
    lines.into_iter().skip(skip).take(height).collect()
}

/// The inbox in full: what needs you first, with the keys at the foot
/// while it has them.
fn draw_inbox(frame: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    frame.render_widget(Clear, area);
    frame.render_widget(inbox_block(app), area);
    let [title, body, foot] = inbox_areas(app, area);
    let waiting = inbox::shown(app)
        .iter()
        .filter(|(e, seen)| !seen && e.kind == Kind::NeedsYou)
        .count();
    let count = if waiting > 0 {
        Span::styled(format!("{waiting} need you"), t.header.fg(t.warning))
    } else {
        Span::raw("")
    };
    heading_bar(frame, Block::bordered().inner(area));
    let mut heading = Line::from(vec![inbox_icon(waiting), Span::styled(" INBOX", t.header)]);
    let pad = usize::from(title.width).saturating_sub(heading.width() + count.width());
    heading.spans.extend([Span::raw(" ".repeat(pad)), count]);
    frame.render_widget(Paragraph::new(heading), title);
    let lines: Vec<Line> = inbox_lines(app, body).into_iter().map(|(l, _)| l).collect();
    frame.render_widget(Paragraph::new(lines), body);
    let keys = if app.rail.is_some() {
        "Enter open  x dismiss  Esc back"
    } else {
        "e inbox  E hide"
    };
    frame.render_widget(Paragraph::new(Line::styled(keys, fg(t.muted))), foot);
}

/// The inbox's three counts of `kinds`, as its strip and the picker draw
/// them: needs you, failed and done, each by its own mark and colour.
const COUNTS: [(&[Kind], &str); 3] = [
    (&[Kind::NeedsYou], "●"),
    (&[Kind::Failed, Kind::Died], "✗"),
    (&[Kind::Done, Kind::Merged], "✓"),
];

fn count_style(mark: &str) -> Style {
    let t = theme();
    match mark {
        "●" => fg(t.warning).add_modifier(Modifier::BOLD),
        "✗" => fg(t.danger),
        _ => fg(t.success),
    }
}

/// `kinds` counted as the picker shows them: only the counts above zero,
/// so a workspace with nothing waiting says nothing.
fn count_spans(kinds: &[Kind]) -> Vec<Span<'static>> {
    COUNTS
        .iter()
        .filter_map(|(of, mark)| {
            let n = kinds.iter().filter(|k| of.contains(k)).count();
            (n > 0).then(|| Span::styled(format!("{mark}{n} "), count_style(mark)))
        })
        .collect()
}

/// Shades the top row of `area` as the list's headings are, so the
/// heading bar runs on across the inbox beside the list. Only its
/// background: the border keeps its own colour, and the text drawn there
/// takes the headings' style itself.
fn heading_bar(frame: &mut Frame, area: Rect) {
    let shade = theme()
        .header
        .bg
        .map_or_else(Style::new, |bg| Style::new().bg(bg));
    frame
        .buffer_mut()
        .set_style(Rect { height: 1, ..area }, shade);
}

/// The inbox's mark in its heading: yellow while `waiting` agents need
/// you, grey when none do.
fn inbox_icon(waiting: usize) -> Span<'static> {
    let t = theme();
    Span::styled(
        "✉",
        t.header.fg(if waiting > 0 { t.warning } else { t.muted }),
    )
}

/// The inbox folded to its counts: unseen entries needing you, failed and
/// done, for a terminal too narrow for it in full.
fn draw_inbox_strip(frame: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let block = Block::bordered().border_style(fg(t.border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    heading_bar(frame, inner);
    let unseen: Vec<Kind> = inbox::shown(app)
        .into_iter()
        .filter(|(_, seen)| !seen)
        .map(|(e, _)| e.kind)
        .collect();
    let waiting = unseen.iter().filter(|k| **k == Kind::NeedsYou).count();
    let mut lines = vec![Line::from(inbox_icon(waiting))];
    for (kinds, mark) in COUNTS {
        let n = unseen.iter().filter(|k| kinds.contains(k)).count();
        let style = if n == 0 {
            fg(t.muted)
        } else {
            count_style(mark)
        };
        lines.push(Line::styled(format!("{mark}{n}"), style));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("e", fg(t.muted)));
    frame.render_widget(
        Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center),
        inner,
    );
}

/// The window's info on `tree`, to fill `inner`: where it is, its
/// agents, what it serves and runs, what its agent last said with its
/// latest steps, and its last action's outcome.
fn detail_text(app: &App, tree: &Tree, inner: Rect) -> Vec<Line<'static>> {
    let t = theme();
    let mut lines = detail_lines(app, tree);
    let item = app.says(tree);
    if let Some(item) = &item {
        lines.extend(said_lines(item, inner.width));
    }
    let last = app.log.iter().rev().find(|e| e.tree == tree.name);
    let fixed = wrapped_height(&lines, inner.width);
    let room = usize::from(inner.height).saturating_sub(fixed);
    if let Some(entry) = last {
        lines.extend(action_lines(entry, room.min(3)));
    }
    let room = usize::from(inner.height).saturating_sub(wrapped_height(&lines, inner.width));
    if let (Some(item), 2..) = (&item, room) {
        let steps = &item.report.steps[item.report.steps.len().saturating_sub(room - 1)..];
        lines.push(Line::styled("steps", fg(t.muted)));
        lines.extend(steps.iter().map(|step| {
            let at = step.at.map_or_else(|| "--:--".into(), reports::clock);
            let state = step.state.map_or("", State::label);
            Line::from(vec![
                Span::styled(format!("{at} "), fg(t.muted)),
                Span::styled(format!("{state:<9} "), fg(t.muted)),
                Span::raw(step.detail.replace('\n', " ")),
            ])
        }));
    }
    lines
}

/// Where the detail pane's values start, after their label.
const INDENT: &str = "          ";

/// What an agent last said, every line, led by its state, wrapped to `width`
/// under its label.
fn said_lines(item: &Item, width: u16) -> Vec<Line<'static>> {
    let t = theme();
    let (glyph, style) = state_style(item.state);
    let live = match item.agent {
        Some(Agent { pid: Some(_), .. }) => "running",
        _ => "not running; a message starts it again",
    };
    let mut lines = vec![Line::from(vec![
        Span::styled("says      ", fg(t.muted)),
        Span::styled(format!("{glyph} {} ", item.state.label()), style),
        Span::styled(
            format!(
                "{} · {} · job {} · {live}",
                item.report.name,
                reports::age(item.report.updated),
                item.report.job
            ),
            fg(t.muted),
        ),
    ])];
    let room = usize::from(width).saturating_sub(INDENT.len()).max(1);
    lines.extend(
        item.text()
            .lines()
            .flat_map(|text| word_wrap(text, room))
            .map(|row| Line::raw(format!("{INDENT}{row}"))),
    );
    lines
}

/// `text` broken at spaces into rows of at most `width` characters; a word
/// longer than a row is cut.
fn word_wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows = vec![String::new()];
    for word in text.split(' ') {
        let row = rows.last_mut().expect("rows starts with one");
        let len = row.chars().count();
        if len > 0 && len + 1 + word.chars().count() > width {
            rows.push(String::new());
        }
        let row = rows.last_mut().expect("just pushed");
        if !row.is_empty() {
            row.push(' ');
        }
        row.push_str(word);
    }
    rows.into_iter()
        .flat_map(|row| wrap_chars(&row, width))
        .filter(|row| !row.is_empty())
        .collect()
}

/// The tree's last action and the end of its output.
fn action_lines(entry: &LogEntry, shown: usize) -> Vec<Line<'static>> {
    let t = theme();
    let (verdict, color) = if entry.is_ok {
        ("ok", t.success)
    } else {
        ("failed", t.danger)
    };
    let mut lines = vec![Line::from(vec![
        Span::styled("last      ", fg(t.muted)),
        Span::styled(
            format!("{} {verdict}", entry.kind.noun()),
            fg(color).add_modifier(Modifier::BOLD),
        ),
    ])];
    let output: Vec<&str> = entry
        .output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines.extend(
        output[output.len().saturating_sub(shown)..]
            .iter()
            .map(|l| Line::styled(format!("{INDENT}{l}"), fg(t.muted))),
    );
    lines
}

/// The workspace picker: every repository treetop has been opened in, this
/// one marked, with how many trees each holds.
fn draw_picker(frame: &mut Frame, app: &App, picker: &Picker) {
    let t = theme();
    let screen = frame.area();
    let width = screen.width.saturating_sub(4).min(96);
    let height = u16::try_from(picker.choices.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = popup::centered(screen, width, height);
    let keys = [("j/k", "move"), ("Enter", "switch"), ("Esc", "close")];
    let block = popup_block(Line::styled(
        " Workspaces ",
        fg(t.title).add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Line::from(fitting_keys(&keys, width.saturating_sub(2))));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let unseen: Vec<Entry> = inbox::shown(app)
        .into_iter()
        .filter(|(_, seen)| !seen)
        .map(|(e, _)| e)
        .collect();
    let rows: Vec<Row> = picker
        .choices
        .iter()
        .map(|choice| {
            let is_here = choice.checkout == app.checkout;
            let held = match choice.held {
                Some(n) => format!("{n} held"),
                None => "not listed yet".into(),
            };
            let mine: Vec<Kind> = unseen
                .iter()
                .filter(|e| match &e.away {
                    Some(away) => away.checkout == choice.checkout,
                    None => is_here,
                })
                .map(|e| e.kind)
                .collect();
            Row::new(vec![
                Cell::from(if is_here { "●" } else { " " }).style(fg(t.accent)),
                Cell::from(choice.name.clone()).style(Style::new().add_modifier(Modifier::BOLD)),
                Cell::from(held).style(fg(t.info)),
                Cell::from(Line::from(count_spans(&mine))),
                Cell::from(choice.checkout.display().to_string()).style(fg(t.muted)),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(1),
        Constraint::Length(20),
        Constraint::Length(14),
        Constraint::Length(12),
        Constraint::Fill(1),
    ];
    let table = Table::new(rows, widths).row_highlight_style(t.cursor);
    let mut state = TableState::default().with_selected(Some(picker.selected));
    frame.render_stateful_widget(table, inner, &mut state);
}

/// The scratch chats `C` lists: a line that starts a new one, then each
/// chat, the newest first, with its age and what it last said.
fn draw_scratch_menu(frame: &mut Frame, app: &App, menu: &ScratchMenu) {
    let t = theme();
    let screen = frame.area();
    let width = screen.width.saturating_sub(4).min(96);
    // Wrapped here, at words, so the box is sized to the rows it takes.
    let error: Option<Vec<Line>> = menu.error.as_ref().map(|error| {
        word_wrap(error, usize::from(width.saturating_sub(2)))
            .into_iter()
            .map(|row| Line::styled(row, fg(t.danger)))
            .collect()
    });
    let error_height = error.as_ref().map_or(0, |lines| lines.len() + 1);
    let height = u16::try_from(menu.chats.len() + 1 + error_height)
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = popup::centered(screen, width, height);
    let keys: &[(&str, &str)] = if menu.is_deleting {
        &[("x", "again deletes it"), ("any key", "keeps it")]
    } else {
        &[("Enter", "open"), ("x x", "delete"), ("Esc", "close")]
    };
    let block = popup_block(Line::styled(
        " Scratch chats ",
        fg(t.title).add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Line::from(fitting_keys(keys, width.saturating_sub(2))));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let mut rows = vec![Row::new(vec![
        Cell::from("+").style(fg(t.accent)),
        Cell::from("new chat").style(Style::new().add_modifier(Modifier::BOLD)),
        Cell::from(""),
        Cell::from("a Claude session for thinking, in no tree").style(fg(t.muted)),
    ])];
    for (at, chat) in menu.chats.iter().enumerate() {
        let is_doomed = menu.is_deleting && menu.selected == at + 1;
        let said = app.scratch_says(&chat.job).unwrap_or_default();
        rows.push(Row::new(vec![
            Cell::from(if is_doomed { "x" } else { " " }).style(fg(t.danger)),
            Cell::from(chat.name.clone()).style(if is_doomed {
                fg(t.danger).add_modifier(Modifier::BOLD)
            } else {
                Style::new().add_modifier(Modifier::BOLD)
            }),
            Cell::from(reports::age(UNIX_EPOCH + Duration::from_secs(chat.created)))
                .style(fg(t.info)),
            Cell::from(said).style(fg(t.muted)),
        ]));
    }
    let widths = [
        Constraint::Length(1),
        Constraint::Length(14),
        Constraint::Length(8),
        Constraint::Fill(1),
    ];
    let [list_area, error_area] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(u16::try_from(error_height).unwrap_or(0)),
    ])
    .areas(inner);
    let table = Table::new(rows, widths).row_highlight_style(t.cursor);
    let mut state = TableState::default().with_selected(Some(menu.selected));
    frame.render_stateful_widget(table, list_area, &mut state);
    if let Some(lines) = error {
        let below = Rect::new(
            error_area.x,
            error_area.y + 1,
            error_area.width,
            error_area.height.saturating_sub(1),
        );
        frame.render_widget(Paragraph::new(lines), below);
    }
}

/// The ports `o`, `p`, `P` and `Q` offer for a tree serving several, each
/// with its number key and the process listening on it; the keys at the
/// foot name what the one that opened the menu will do.
fn draw_port_menu(frame: &mut Frame, menu: &PortMenu) {
    let t = theme();
    let screen = frame.area();
    let width = screen.width.saturating_sub(4).min(48);
    let height = u16::try_from(menu.ports.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = popup::centered(screen, width, height);
    let does = match menu.reach {
        Reach::Browser => "open",
        Reach::Phone => "share",
        Reach::Public => "publish",
        Reach::Code => "show",
    };
    let keys = [("1-9", does), ("Enter", does), ("Esc", "close")];
    let block = popup_block(Line::styled(
        format!(" Tree {} serves ", menu.tree),
        fg(t.title).add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Line::from(fitting_keys(&keys, width.saturating_sub(2))));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let rows: Vec<Row> = menu
        .ports
        .iter()
        .enumerate()
        .map(|(at, (port, name))| {
            let number = if at < 9 {
                format!(" {} ", at + 1)
            } else {
                String::new()
            };
            Row::new(vec![
                Cell::from(Span::styled(number, t.badge)),
                Cell::from(format!("localhost:{port}"))
                    .style(fg(t.info).add_modifier(Modifier::BOLD)),
                Cell::from(name.clone()).style(fg(t.muted)),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(3),
        Constraint::Length(16),
        Constraint::Fill(1),
    ];
    let table = Table::new(rows, widths).row_highlight_style(t.cursor);
    let mut state = TableState::default().with_selected(Some(menu.selected));
    frame.render_stateful_widget(table, inner, &mut state);
}

/// The port `p` shares, or `P` publishes, as a QR code of its address for
/// the phone's camera to scan, with the address under it; or that it is
/// being shared, or why it could not be.
fn draw_phone(frame: &mut Frame, phone: &Phone) {
    let t = theme();
    let screen = frame.area();
    let (place, sharing, share) = if phone.is_public {
        ("on the internet", "publishing", "publish")
    } else {
        ("on your phone", "sharing", "share")
    };
    let title = Line::styled(
        format!(" Tree {} {place} ", phone.tree),
        fg(t.title).add_modifier(Modifier::BOLD),
    );
    let local = format!("localhost:{}", phone.port);
    let (url, code) = match &phone.shared {
        Some(Ok((url, code))) => (url, code),
        pending_or_failed => {
            let (said, colour) = match pending_or_failed {
                Some(Err(err)) => (format!("could not {share} {local}: {err}"), t.danger),
                _ => (format!("{sharing} {local}..."), t.muted),
            };
            return draw_phone_note(frame, title, &[("Esc", "close")], said, colour);
        }
    };
    let keys: &[(&str, &str)] = if phone.is_public {
        &[("x", "stop publishing"), ("Esc", "close")]
    } else {
        &[("x", "stop sharing"), ("Esc", "close")]
    };
    // Each cell is two modules, one over the other, drawn as an upper half
    // block: its foreground the top module, its background the bottom one.
    // Under the code, a blank line, the address, and how to use it.
    let note = if phone.is_public {
        "anyone with this link can open it"
    } else {
        "scan it with the phone's camera"
    };
    let text_width = url.chars().count().max(note.len());
    let fits = |quiet: usize| {
        let side = code.width + 2 * quiet;
        let (cols, rows) = (side.max(text_width) + 5, side.div_ceil(2) + 5);
        cols <= usize::from(screen.width) && rows <= usize::from(screen.height)
    };
    let Some(quiet) = (1..=QUIET_ZONE).rev().find(|&quiet| fits(quiet)) else {
        let said = format!("the screen is too small for the code; {url}");
        return draw_phone_note(frame, title, keys, said, t.text);
    };
    let side = code.width + 2 * quiet;
    let rows = side.div_ceil(2);
    // As wide as the code or the address, with a column of air each side,
    // and a column more where that leaves the code off centre.
    let inside = side.max(text_width) + 2;
    let inside = inside + (inside - side) % 2;
    let width = u16::try_from(inside + 2).unwrap_or(u16::MAX);
    let height = u16::try_from(rows + 5).unwrap_or(u16::MAX);
    let area = popup::centered(screen, width, height);
    let block =
        popup_block(title).title_bottom(Line::from(fitting_keys(keys, width.saturating_sub(2))));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    // Black on white whatever the theme, which every camera reads.
    let colour = |x: usize, y: usize| {
        let (x, y) = (x as isize - quiet as isize, y as isize - quiet as isize);
        if code.is_dark(x, y) {
            Color::Black
        } else {
            Color::White
        }
    };
    let left = inner.x + (inner.width - side as u16) / 2;
    let buffer = frame.buffer_mut();
    for row in 0..rows {
        for x in 0..side {
            let position = Position::new(left + x as u16, inner.y + row as u16);
            let bottom = if 2 * row + 1 < side {
                colour(x, 2 * row + 1)
            } else {
                Color::Reset
            };
            if let Some(cell) = buffer.cell_mut(position) {
                cell.set_symbol("▀")
                    .set_style(Style::new().fg(colour(x, 2 * row)).bg(bottom));
            }
        }
    }
    let under = Rect::new(inner.x, inner.y + rows as u16 + 1, inner.width, 2);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(url.clone(), fg(t.info).add_modifier(Modifier::BOLD)),
            Line::styled(note, fg(t.muted)),
        ])
        .centered(),
        under,
    );
}

/// The phone's popup with a line of text in place of the code, wrapped.
fn draw_phone_note(
    frame: &mut Frame,
    title: Line<'static>,
    keys: &[(&str, &str)],
    said: String,
    colour: Color,
) {
    let screen = frame.area();
    let width = screen.width.saturating_sub(4).min(64);
    let text = Paragraph::new(said)
        .style(fg(colour))
        .wrap(Wrap { trim: true });
    let lines = u16::try_from(text.line_count(width.saturating_sub(4))).unwrap_or(1);
    let area = popup::centered(screen, width, lines.saturating_add(2));
    let block = popup_block(title)
        .title_bottom(Line::from(fitting_keys(keys, width.saturating_sub(2))))
        .padding(Padding::horizontal(1));
    frame.render_widget(Clear, area);
    frame.render_widget(text.block(block), area);
}

/// A popup's frame over the dimmed table: a border in the accent colour and
/// its title. Callers put the keys where they read best.
fn popup_block(title: Line<'static>) -> Block<'static> {
    Block::bordered()
        .title(title)
        .border_style(fg(theme().accent))
        .style(fg(theme().text))
}

/// `text` broken into rows of `width` characters, at its own line breaks
/// and wherever a line runs past the width, so the cursor's place is known.
fn wrap_chars(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            rows.push(String::new());
        }
        rows.extend(chars.chunks(width).map(|chunk| chunk.iter().collect()));
    }
    if rows.last().is_some_and(|row| row.chars().count() == width) {
        rows.push(String::new());
    }
    rows
}

/// Rows the compose box gives its text, growing with it.
const COMPOSE_ROWS: (u16, u16) = (7, 16);

/// The compose box in the middle of the screen: who the text goes to, the
/// text, wrapped and growing as it is typed, and the keys.
fn draw_prompt(frame: &mut Frame, screen: Rect, prompt: &Prompt) {
    let t = theme();
    let width = screen.width.saturating_sub(4).min(116);
    let text_width = usize::from(width.saturating_sub(4));
    let rows = wrap_chars(&prompt.text, text_width);
    // A handoff is long and read over before it goes, so its box takes
    // what the screen has.
    let most = if prompt.is_handoff() {
        screen.height.saturating_sub(8).max(COMPOSE_ROWS.0)
    } else {
        COMPOSE_ROWS.1
    };
    let text_height = u16::try_from(rows.len())
        .unwrap_or(u16::MAX)
        .clamp(COMPOSE_ROWS.0, most);
    // Borders, then the branch, the base for a fresh tree, and a blank line
    // when the box names a branch.
    let fields = u16::from(prompt.names_branch()) + u16::from(prompt.names_base());
    let chrome = 2 + fields + u16::from(fields > 0);
    let area = popup::centered(screen, width, text_height + chrome);
    let (title, placeholder) = match (prompt.target(), &prompt.purpose) {
        (Some(Target::Ask), _) => (
            " Ask the assistant ".to_string(),
            "a question about what is on screen, or @17 for that tree's agent",
        ),
        (Some(Target::NewTree), _) => (
            " New agent in a fresh tree ".to_string(),
            "the task, or nothing to start it idle and open its chat",
        ),
        (None, Purpose::Message { to }) => (
            match to.as_slice() {
                [one] => format!(" Message {} in {} ", one.agent, one.tree),
                many => format!(" Message {} agents ", many.len()),
            },
            "what to tell it",
        ),
        (None, Purpose::Dispatch { .. }) => (" New agent ".to_string(), ""),
        (None, Purpose::Start { tree }) => (
            if tree.is_main {
                " New agent in the main checkout ".to_string()
            } else {
                format!(" New agent in tree {} ", tree.name)
            },
            "the task, or nothing to start it idle and open its chat",
        ),
        (None, Purpose::Rename { tree }) => (
            format!(" Rename the branch of tree {} ", tree.name),
            "the branch's new name",
        ),
        (None, Purpose::Handoff { tree, from }) => {
            // An agent's name can be long, so the title is cut short of the
            // box's corner rather than running into it.
            let most = usize::from(width.saturating_sub(4));
            let mut title = format!(" Handoff from {}, tree {} ", from.agent, tree.name);
            if title.chars().count() > most {
                title = format!(
                    " {}… ",
                    title
                        .trim()
                        .chars()
                        .take(most.saturating_sub(3))
                        .collect::<String>()
                );
            }
            (title, "the new agent's prompt")
        }
    };
    let mut keys = vec![("Enter", "send"), ("Esc", "cancel")];
    let is_editing_field = prompt.is_editing_branch || prompt.is_editing_base;
    if !matches!(prompt.purpose, Purpose::Rename { .. }) && !is_editing_field {
        keys.insert(1, ("Alt+Enter", "new line"));
    }
    // A handoff is read over rather than typed, so the editor comes before
    // a new line, and after the way out.
    if prompt.is_handoff() {
        keys.retain(|(key, _)| *key != "Alt+Enter");
        keys.extend([("^E", "editor"), ("Alt+Enter", "new line")]);
    }
    if prompt.is_editing_base {
        keys.insert(0, ("type", "narrow"));
        keys.insert(0, ("←→", "pick"));
    }
    if prompt.names_base() {
        keys.insert(0, ("↑↓", "branch, base"));
    } else if prompt.names_branch() {
        keys.insert(0, ("↑↓", "branch"));
    }
    let block = popup_block(Line::styled(
        title,
        fg(t.title).add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Line::from(fitting_keys(&keys, width.saturating_sub(2))));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let inner = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    );
    let mut text_area = inner;
    if prompt.names_branch() {
        let branch_row = Rect::new(inner.x, inner.y, inner.width, 1);
        frame.render_widget(Paragraph::new(branch_line(prompt)), branch_row);
        if prompt.is_editing_branch {
            let typed = prompt.branch.as_deref().map_or(0, |b| b.chars().count());
            let at = u16::try_from(FIELD_LABEL + typed).unwrap_or(u16::MAX);
            frame.set_cursor_position(Position::new(
                (branch_row.x + at).min(branch_row.right().saturating_sub(1)),
                branch_row.y,
            ));
        }
        if prompt.names_base() {
            let base_row = Rect::new(inner.x, inner.y + 1, inner.width, 1);
            frame.render_widget(Paragraph::new(base_line(prompt)), base_row);
            // The cursor sits after what is typed, which narrows the list.
            if prompt.is_editing_base {
                let typed = prompt.base_typed.chars().count();
                let at = u16::try_from(FIELD_LABEL + typed).unwrap_or(u16::MAX);
                frame.set_cursor_position(Position::new(
                    (base_row.x + at).min(base_row.right().saturating_sub(1)),
                    base_row.y,
                ));
            }
        }
        text_area = Rect::new(
            inner.x,
            inner.y + fields + 1,
            inner.width,
            inner.height.saturating_sub(fields + 1),
        );
    }
    if prompt.text.is_empty() {
        frame.render_widget(
            Paragraph::new(placeholder)
                .wrap(Wrap { trim: false })
                .style(fg(t.muted).add_modifier(Modifier::ITALIC)),
            text_area,
        );
        if !is_editing_field {
            frame.set_cursor_position(Position::new(text_area.x, text_area.y));
        }
        return;
    }
    // The end of a long text stays in view, since that is where it is
    // typed, unless it is to be read from the top.
    let shown = usize::from(text_area.height);
    let first = if prompt.is_from_top {
        0
    } else {
        rows.len().saturating_sub(shown)
    };
    let lines: Vec<Line> = rows[first..]
        .iter()
        .take(shown)
        .map(|r| Line::raw(r.clone()))
        .collect();
    frame.render_widget(Paragraph::new(lines), text_area);
    // The cursor shows only where the end is in view, since it types there.
    if is_editing_field || first + shown < rows.len() {
        return;
    }
    let last = rows.last().map_or(0, |row| row.chars().count());
    let cursor_row = u16::try_from(rows.len() - 1 - first).unwrap_or(0);
    frame.set_cursor_position(Position::new(
        text_area.x + u16::try_from(last).unwrap_or(0),
        text_area.y + cursor_row,
    ));
}

/// The width of a field's label in the compose box, `branch ` and `base   `
/// alike, so the two values line up.
const FIELD_LABEL: usize = 7;

/// The compose box's base line: the branch a fresh tree is cut from, the
/// pool's until another is picked. While it has the focus it shows what is
/// typed, then the match picked between arrows and where it stands among
/// the matches; a typed name that matches no branch says so.
fn base_line(prompt: &Prompt) -> Line<'static> {
    let t = theme();
    let label = Span::styled("base   ", fg(t.muted));
    let typed = prompt.base_typed.clone();
    let unmatched = || {
        Span::styled(
            format!("no branch matches \"{}\"", typed.trim()),
            fg(t.danger).add_modifier(Modifier::BOLD),
        )
    };
    let is_pools = prompt.base_name().is_none();
    if !prompt.is_editing_base {
        let shown = match prompt.base_shown() {
            None => unmatched(),
            Some(name) if is_pools => Span::styled(
                format!("{name}, the pool's"),
                fg(t.muted).add_modifier(Modifier::ITALIC),
            ),
            Some(name) => Span::raw(name.to_string()),
        };
        return Line::from(vec![label, shown]);
    }
    let mut spans = vec![
        label,
        Span::styled(typed.clone(), fg(t.accent).add_modifier(Modifier::BOLD)),
    ];
    if !typed.is_empty() {
        spans.push(Span::raw("  "));
    }
    match prompt.base_shown() {
        None => spans.push(unmatched()),
        Some(name) => {
            let matches = prompt.base_matches().len().max(1);
            let place = if typed.trim().is_empty() {
                format!("  {}/{matches}", prompt.base_at + 1)
            } else {
                format!("  {}/{matches} matching", prompt.base_at + 1)
            };
            spans.extend([
                Span::styled("‹ ", fg(t.accent)),
                Span::styled(name.to_string(), fg(t.accent).add_modifier(Modifier::BOLD)),
                Span::styled(" ›", fg(t.accent)),
                Span::styled(
                    format!("{place}{}", if is_pools { ", the pool's" } else { "" }),
                    fg(t.muted),
                ),
            ]);
        }
    }
    Line::from(spans)
}

/// The compose box's branch line: the branch a new agent will start on, as
/// edited, else named from the task when it starts.
fn branch_line(prompt: &Prompt) -> Line<'static> {
    let t = theme();
    let name = if prompt.is_editing_branch {
        Span::styled(
            prompt.branch.clone().unwrap_or_default(),
            fg(t.accent).add_modifier(Modifier::BOLD),
        )
    } else {
        match &prompt.branch {
            Some(name) => Span::raw(name.clone()),
            None => Span::styled(
                "named from the task when it starts",
                fg(t.muted).add_modifier(Modifier::ITALIC),
            ),
        }
    };
    Line::from(vec![Span::styled("branch ", fg(t.muted)), name])
}

/// The `?` overlay: every key in its groups, in the HELP_COLUMNS columns
/// side by side, or fewer where the screen is too narrow for them.
fn draw_help(frame: &mut Frame, help: &[(&str, &[(&str, &str)])]) {
    let t = theme();
    let (lines, width) = help_layout(help, usize::from(frame.area().width));
    let width = u16::try_from(width).unwrap_or(u16::MAX);
    let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);
    let area = popup::centered(frame.area(), width, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            popup_block(Line::styled(
                " Keys ",
                fg(t.title).add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Line::styled(" any key closes ", fg(t.muted))),
        ),
        area,
    );
}

/// The overlay's lines and the width of its box: the most columns of
/// HELP_COLUMNS, then half as many, then one, that fit `room` columns of
/// screen. Every line stays whole; a narrow screen gets a taller box.
fn help_layout(help: &[(&str, &[(&str, &str)])], room: usize) -> (Vec<Line<'static>>, usize) {
    const GAP: usize = 3;
    let widest = |lines: &[Line]| lines.iter().map(Line::width).max().unwrap_or(0);
    let mut layout = (Vec::new(), 0);
    for together in [1, 2, HELP_COLUMNS.len()] {
        // Each `together` neighbours of HELP_COLUMNS make one column, and the
        // last takes whatever groups are left.
        let sizes: Vec<usize> = HELP_COLUMNS
            .chunks(together)
            .map(|s| s.iter().sum())
            .collect();
        let mut columns: Vec<Vec<Line>> = Vec::new();
        let mut rest = help;
        for (at, size) in sizes.iter().enumerate() {
            let take = if at + 1 == sizes.len() {
                rest.len()
            } else {
                (*size).min(rest.len())
            };
            let (column, after) = rest.split_at(take);
            columns.push(help_lines(column));
            rest = after;
        }
        let widths: Vec<usize> = columns.iter().map(|column| widest(column)).collect();
        // Each line starts with a space of its own, so the border wants one
        // more column of air at the right and one for each side's border.
        let width = widths.iter().sum::<usize>() + GAP * (widths.len() - 1) + 3;
        let rows = columns.iter().map(Vec::len).max().unwrap_or(0);
        let lines = (0..rows)
            .map(|at| {
                let mut spans: Vec<Span> = Vec::new();
                let mut owed = 0;
                for (column, column_width) in columns.iter().zip(&widths) {
                    match column.get(at).filter(|line| line.width() > 0) {
                        Some(line) => {
                            spans.push(Span::raw(" ".repeat(owed)));
                            spans.extend(line.spans.iter().cloned());
                            owed = column_width + GAP - line.width();
                        }
                        None => owed += column_width + GAP,
                    }
                }
                Line::from(spans)
            })
            .collect();
        layout = (lines, width);
        if width <= room {
            break;
        }
    }
    layout
}

/// One column of the overlay: each group's name over its keys, a space, the
/// key in seven columns, a space, then what it does, with a blank line
/// between groups.
fn help_lines(groups: &[(&str, &[(&str, &str)])]) -> Vec<Line<'static>> {
    let t = theme();
    let mut lines: Vec<Line> = Vec::new();
    for (name, keys) in groups {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            format!(" {name}"),
            fg(t.title).add_modifier(Modifier::BOLD),
        ));
        for (key, does) in *keys {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {key:<7} "),
                    fg(t.accent).add_modifier(Modifier::BOLD),
                ),
                Span::raw((*does).to_string()),
            ]));
        }
    }
    lines
}

/// A dialog's frame: a border in `color` and its title in the theme's title
/// colour, as Neovim draws a float's `FloatTitle`.
fn dialog(title: String, color: Color) -> Block<'static> {
    Block::bordered()
        .title(Span::styled(
            title,
            fg(theme().title).add_modifier(Modifier::BOLD),
        ))
        .border_style(fg(color))
}

/// What returning a tree throws away, in words under its branch, since this
/// dialog is the one place a tree is returned from under work.
fn risk(tree: &Tree) -> Vec<Span<'static>> {
    let t = theme();
    let mut spans = vec![Span::raw(RETURN_INDENT)];
    match tree.work {
        None => spans.push(Span::styled(
            "unknown changes: git cannot read it",
            fg(t.warning),
        )),
        Some(w) if w.changed + w.unpushed > 0 => spans.push(Span::styled(
            format!("{} uncommitted, {} unpushed", w.changed, w.unpushed),
            fg(t.warning),
        )),
        Some(_) => spans.push(Span::styled("clean", fg(t.success))),
    }
    if let Some(ship) = tree.ship.as_ref().filter(|s| s.stage == Stage::Merged) {
        let number = ship.number().unwrap_or_else(|| "its PR".into());
        spans.push(Span::styled(
            format!(", merged as {number}"),
            stage_style(Stage::Merged),
        ));
    }
    spans
}

/// Where a tree's lines under its branch start in the return dialog.
const RETURN_INDENT: &str = "      ";

/// The live processes a return would stop, by pid, as lines under the
/// tree's, wrapped to `width`; none when nothing runs in it.
fn live_lines(tree: &Tree, width: usize) -> Vec<Line<'static>> {
    let pids = guard::live_pids(tree, &HashSet::new());
    if pids.is_empty() {
        return Vec::new();
    }
    let pids: Vec<String> = pids.iter().map(u32::to_string).collect();
    let text = format!("live: pid {}", pids.join(", "));
    word_wrap(&text, width.saturating_sub(RETURN_INDENT.len()))
        .into_iter()
        .map(|row| {
            Line::from(Span::styled(
                format!("{RETURN_INDENT}{row}"),
                fg(theme().danger),
            ))
        })
        .collect()
}

/// Asks before a ship run, which pushes the branch and opens its PR.
fn draw_shipping(frame: &mut Frame, shipping: &Shipping) {
    let t = theme();
    let tree = &shipping.tree;
    let width: u16 = 80;
    let room = usize::from(width.saturating_sub(4 + 9));
    let mut lines = vec![
        Line::from(vec![
            Span::styled(" branch   ", fg(t.muted)),
            Span::raw(tree.branch.clone().unwrap_or_default()),
        ]),
        Line::from(vec![
            Span::styled(" through  ", fg(t.muted)),
            Span::raw(shipping.plugin.clone()),
        ]),
    ];
    for (i, row) in word_wrap(&shipping.intent.replace('\n', " "), room)
        .into_iter()
        .take(4)
        .enumerate()
    {
        let label = if i == 0 { " intent   " } else { "          " };
        lines.push(Line::from(vec![
            Span::styled(label, fg(t.muted)),
            Span::raw(row),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw(
        " The run validates the branch, then pushes it and opens its PR.",
    ));
    lines.push(Line::raw(" y starts it; any other key cancels."));
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = popup::centered(frame.area(), width, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(dialog(format!(" Ship tree {}? ", tree.name), t.warning)),
        area,
    );
}

fn draw_confirm(frame: &mut Frame, pending: &Pending) {
    let width: u16 = 80;
    let mut lines = Vec::new();
    for t in &pending.trees {
        lines.push(Line::raw(format!(
            " {:>3}  {}",
            t.name,
            t.branch.as_deref().unwrap_or("(detached)")
        )));
        lines.push(Line::from(risk(t)));
        if let Some(run) = pending.runs.get(&t.path) {
            lines.push(Line::styled(
                format!("      keel run {} holds it", run.summary()),
                fg(theme().danger).add_modifier(Modifier::BOLD),
            ));
        }
        lines.extend(live_lines(t, usize::from(width - 4)));
    }
    let is_live = pending
        .trees
        .iter()
        .any(|t| !guard::live_pids(t, &HashSet::new()).is_empty());
    lines.push(Line::raw(""));
    lines.push(Line::raw(
        " Uncommitted changes are discarded and unpushed commits left behind.",
    ));
    if is_live {
        let has_agent = pending
            .trees
            .iter()
            .any(|t| t.agents.iter().any(|a| a.pid.is_some()));
        let text = if has_agent {
            " The live processes above are stopped, an agent's session among them."
        } else {
            " The live processes above are stopped."
        };
        lines.push(Line::styled(text, fg(theme().danger)));
    }
    if !pending.runs.is_empty() {
        lines.push(Line::styled(
            " A keel run is not finished: its steps are left undone and it stays",
            fg(theme().danger),
        ));
        lines.push(Line::styled(
            " listed until `keel run stop`. keel itself never returns such a tree.",
            fg(theme().danger),
        ));
    }
    lines.push(Line::raw(
        " y returns them to the pool, D destroys them from disk after a preview;",
    ));
    lines.push(Line::raw(" any other key cancels."));
    let title = format!(" Return {} tree(s)? ", pending.trees.len());
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = popup::centered(frame.area(), width, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(dialog(title, theme().warning)),
        area,
    );
}

/// treehouse's own dry run for one tree, scrollable, with the verdict keys.
fn draw_review(frame: &mut Frame, review: &Review, remaining: usize) {
    let full = frame.area();
    // Sized to the preview plus its borders and a blank line, up to 70% of the
    // screen; a longer preview scrolls.
    let lines = u16::try_from(review.preview.lines().count()).unwrap_or(u16::MAX);
    let height = lines.saturating_add(3).max(6).min(full.height * 7 / 10);
    let area = popup::centered(full, full.width * 4 / 5, height);
    let tree = &review.tree;
    let title = format!(
        " Destroy tree {} ({})? ",
        tree.name,
        tree.branch.as_deref().unwrap_or("detached")
    );
    let more = match remaining {
        0 => String::new(),
        n => format!("   {n} more to review"),
    };
    let keys = format!(" y destroys   n keeps it   j/k scroll{more} ");
    let preview = if review.preview.trim().is_empty() {
        "treehouse printed no preview".to_string()
    } else {
        review.preview.clone()
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(preview)
            .wrap(Wrap { trim: false })
            .scroll((review.scroll, 0))
            .block(dialog(title, theme().danger).title_bottom(keys)),
        area,
    );
}

/// One half of a split row: the line number, then the text scrolled sideways
/// by `hscroll` columns, with the chars that differ from its pair on `word`.
fn side_line(
    side: Option<&Side>,
    (line, word): (Color, Color),
    number_width: usize,
    hscroll: u16,
) -> Paragraph<'static> {
    let palette = &theme().diff;
    let Some(side) = side else {
        return Paragraph::new("");
    };
    let on_change = side.is_change.then_some(palette.change_text).flatten();
    let mut spans = vec![Span::styled(
        format!("{:>number_width$} ", side.number),
        fg(on_change.unwrap_or(palette.number)),
    )];
    // Each char takes its token's colour, and the changed words their
    // background over it; a run of chars styled alike is one span.
    let (from, to) = side.emphasis.unwrap_or((0, 0));
    let syntax = palette.syntax.as_ref();
    let mut tokens = side.tokens.iter().peekable();
    let mut run = (String::new(), Style::new());
    for (i, c) in side.text.chars().enumerate().skip(usize::from(hscroll)) {
        while tokens.next_if(|(range, _)| range.end <= i).is_some() {}
        let mut style = match (syntax, tokens.peek()) {
            (Some(syntax), Some((range, token))) if range.start <= i => syntax.style(*token),
            _ => Style::new(),
        };
        if (from..to).contains(&i) {
            style = style.bg(word);
            if style.fg.is_none() {
                style = style.fg(palette.word_text);
            }
        }
        if style != run.1 && !run.0.is_empty() {
            let (text, style) = std::mem::take(&mut run);
            spans.push(Span::styled(text, style));
        }
        run.1 = style;
        run.0.push(c);
    }
    spans.push(Span::styled(run.0, run.1));
    let style = fg(on_change.unwrap_or(palette.text));
    let style = if side.is_change {
        style.bg(line)
    } else {
        style
    };
    Paragraph::new(Line::from(spans)).style(style)
}

fn change_letter(change: Change) -> (&'static str, Color) {
    let t = theme();
    match change {
        Change::Added => ("A", t.success),
        Change::Deleted => ("D", t.danger),
        Change::Modified => ("M", t.info),
        Change::Renamed => ("R", t.title),
    }
}

/// A file's header bar: its change, path and line counts.
fn file_header(file: &FileDiff) -> Paragraph<'static> {
    let t = theme();
    let (letter, color) = change_letter(file.change);
    let path = match &file.from {
        Some(from) => format!("{from} -> {}", file.path),
        None => file.path.clone(),
    };
    Paragraph::new(Line::from(vec![
        Span::styled(format!(" {letter} "), fg(color)),
        Span::styled(format!(" {path}  "), fg(t.diff.path)),
        Span::styled(format!("+{}", file.additions), fg(t.success)),
        Span::raw(" "),
        Span::styled(format!("-{}", file.deletions), fg(t.danger)),
    ]))
    .style(t.diff.header)
}

/// The visible slice of the one scroll through every file. Only these rows
/// are built each frame, so a diff of fifty thousand lines scrolls as fast
/// as one of fifty. While the top row is inside a file, that file's header
/// stays pinned above it, so the path is always on screen.
fn draw_diff_rows(frame: &mut Frame, layout: &DiffLayout, view: &DiffView, body: Rect) {
    let palette = &theme().diff;
    let diff = &layout.diff;
    let start = view.scroll.min(layout.rows.len().saturating_sub(1));
    let pinned = match layout.rows.get(start) {
        Some(ViewRow::Line(f, _) | ViewRow::Binary(f)) => Some(*f),
        _ => None,
    };
    let body = match pinned {
        Some(f) if body.height > 1 => {
            frame.render_widget(
                file_header(&diff.files[f]),
                Rect::new(body.x, body.y, body.width, 1),
            );
            Rect::new(body.x, body.y + 1, body.width, body.height - 1)
        }
        _ => body,
    };
    let visible = &layout.rows[start..(start + usize::from(body.height)).min(layout.rows.len())];
    let widest = visible
        .iter()
        .filter_map(|row| match row {
            ViewRow::Line(f, r) => match &diff.files[*f].rows[*r] {
                DiffRow::Pair(l, r) => Some(l.iter().chain(r).map(|s| s.number).max().unwrap_or(0)),
                DiffRow::Hunk(_) => None,
            },
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let number_width = widest.to_string().len().max(3);
    let half = body.width.saturating_sub(1) / 2;
    for (offset, row) in visible.iter().enumerate() {
        let y = body.y + u16::try_from(offset).unwrap_or(u16::MAX);
        let full = Rect::new(body.x, y, body.width, 1);
        match row {
            ViewRow::Spacer => {}
            ViewRow::File(f) => frame.render_widget(file_header(&diff.files[*f]), full),
            ViewRow::Binary(_) => frame.render_widget(
                Paragraph::new("    binary file: contents not shown").style(fg(palette.hunk)),
                full,
            ),
            ViewRow::Line(f, r) => match &diff.files[*f].rows[*r] {
                DiffRow::Hunk(header) => frame
                    .render_widget(Paragraph::new(header.clone()).style(fg(palette.hunk)), full),
                DiffRow::Pair(left, right) => {
                    let left_area = Rect::new(body.x, y, half, 1);
                    let right_area =
                        Rect::new(body.x + half + 1, y, body.width.saturating_sub(half + 1), 1);
                    frame.render_widget(
                        side_line(
                            left.as_ref(),
                            (palette.removed, palette.removed_word),
                            number_width,
                            view.hscroll,
                        ),
                        left_area,
                    );
                    frame.render_widget(
                        Paragraph::new("│").style(fg(palette.divider)),
                        Rect::new(body.x + half, y, 1, 1),
                    );
                    frame.render_widget(
                        side_line(
                            right.as_ref(),
                            (palette.added, palette.added_word),
                            number_width,
                            view.hscroll,
                        ),
                        right_area,
                    );
                }
            },
        }
    }
}

/// The side-by-side diff of one tree against its base, in `area`, the
/// window's diff tab.
fn draw_diff_view(frame: &mut Frame, app: &App, view: &DiffView, area: Rect) {
    let t = theme();
    // What the diff is sits in a bar on the window's first row, and the keys
    // in one on its last, both drawn like a file header.
    let [bar, rest] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let [body, key_row] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(rest);
    let tree = app.trees.iter().find(|tree| tree.path == view.path);
    let mut title = vec![
        Span::styled(" diff ", t.badge),
        Span::styled(
            format!(
                " tree {}  {}",
                tree.map_or("?", |tree| tree.name.as_str()),
                tree.and_then(|tree| tree.branch.as_deref())
                    .unwrap_or("(detached)")
            ),
            Style::new().add_modifier(Modifier::BOLD),
        ),
    ];
    let loaded = app.diffs.get(&view.path).map(|d| &d.layout);
    if let Some(Ok(layout)) = loaded {
        let diff = &layout.diff;
        let (adds, dels) = diff
            .files
            .iter()
            .fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions));
        title.push(Span::raw(format!(
            "  vs {}   {} files  ",
            diff.base,
            diff.files.len()
        )));
        title.push(Span::styled(format!("+{adds}"), fg(t.success)));
        title.push(Span::raw(" "));
        title.push(Span::styled(format!("-{dels} "), fg(t.danger)));
        if let Some(index) = layout.file_starts.iter().rposition(|&s| s <= view.scroll) {
            // The file's path is in the header pinned under the title.
            title.push(Span::raw(format!(
                "  file {}/{} ",
                index + 1,
                diff.files.len()
            )));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(title)).style(t.diff.header), bar);
    frame.render_widget(
        Paragraph::new(Line::from(fitting_keys(&DIFF_KEYS, key_row.width))).style(t.diff.header),
        key_row,
    );
    match loaded {
        None => frame.render_widget(Paragraph::new(" reading the diff..."), body),
        Some(Err(err)) => {
            frame.render_widget(Paragraph::new(format!(" {err}")).style(fg(t.danger)), body)
        }
        Some(Ok(layout)) if layout.rows.is_empty() => frame.render_widget(
            Paragraph::new(format!(" No changes against {}.", layout.diff.base))
                .style(fg(t.success)),
            body,
        ),
        Some(Ok(layout)) => draw_diff_rows(frame, layout, view, body),
    }
}

/// The keys of each PR tab, by `Tab::index`.
// The tab row numbers the tabs, so the keys leave `1`-`4` out, and Esc
// comes before what a narrow bar can drop.
const PR_KEYS: [&[(&str, &str)]; 4] = [
    &[
        ("j/k", "scroll"),
        ("O", "GitHub"),
        ("Esc", "back"),
        ("^R", "reload"),
    ],
    &[
        ("j/k", "check"),
        ("f", "send to agent"),
        ("Enter", "on GitHub"),
        ("Esc", "back"),
        ("^R", "reload"),
    ],
    &[
        ("j/k", "thread"),
        ("d/u", "scroll"),
        ("f", "send to agent"),
        ("Esc", "back"),
        ("O", "GitHub"),
        ("^R", "reload"),
    ],
    &[
        ("j/k", "move"),
        ("d/u", "scroll"),
        ("f", "send"),
        ("F", "send all"),
        ("z", "nitpicks"),
        ("Esc", "back"),
        ("^R", "reload"),
    ],
];

/// A tree's PR in `area`, the window's PR tab: a bar naming it, its tabs,
/// the tab on show, the last thing said, and the tab's keys.
fn draw_pr_view(frame: &mut Frame, app: &App, popup: &PrPopup, area: Rect) {
    let t = theme();
    let tree = app.trees.iter().find(|tree| tree.path == popup.path);
    let read = app.prs.get(&popup.path);
    let answer = read.and_then(|r| r.pr.as_ref());
    let pr = answer.and_then(|a| a.as_ref().ok()?.as_ref());
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let mut status = Vec::new();
    // Over the last read, whatever it said; the first read says so below.
    if answer.is_some() && read.is_some_and(|r| r.is_reading) {
        status.push(Span::styled("reading… ", fg(t.muted)));
    }
    if let Some(ship) = tree.and_then(|t| t.ship.as_ref()) {
        status.push(Span::styled(
            format!("{} ", ship.label),
            stage_style(ship.stage),
        ));
    }
    let status_width: usize = status.iter().map(Span::width).sum();
    let mut title = vec![Span::styled(" pr ", t.badge)];
    match pr {
        Some(pr) => {
            // The title takes what the bar has left beside the badge, the
            // tree and the status, cut with an ellipsis; the branches follow
            // where they fit.
            let room = usize::from(area.width).saturating_sub(17 + status_width);
            let named = fit(&format!(" #{} {}", pr.number, pr.title), room);
            let left = room.saturating_sub(Line::raw(named.as_str()).width());
            title.push(Span::styled(named, bold));
            let branches = format!("  {} → {}", pr.branch, pr.base);
            if Line::raw(branches.as_str()).width() <= left {
                title.push(Span::styled(branches, fg(t.muted)));
            }
        }
        None => title.push(Span::styled(
            format!(
                " {}",
                tree.and_then(|t| t.branch.as_deref())
                    .unwrap_or("(detached)")
            ),
            bold,
        )),
    }
    if let Some(tree) = tree {
        title.push(Span::styled(format!("   tree {} ", tree.name), fg(t.muted)));
    }
    let [bar, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let title = Line::from(title);
    let status = Line::from(status);
    let fits = title.width() + status.width() <= usize::from(bar.width);
    frame.render_widget(Paragraph::new(title).style(t.diff.header), bar);
    if fits {
        frame.render_widget(Paragraph::new(status.right_aligned()), bar);
    }
    let note = app.message.as_deref();
    let [tabs_row, content, note_row, key_row] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(u16::from(note.is_some())),
        Constraint::Length(1),
    ])
    .areas(body);
    let tab = if pr.is_some() {
        popup.tab
    } else {
        Tab::Overview
    };
    let keys: Vec<(&str, &str)> = match pr {
        // `z` only where there are nitpicks to show.
        Some(pr) => PR_KEYS[tab.index()]
            .iter()
            .filter(|(key, _)| *key != "z" || pr_view::nitpick_count(pr) > 0)
            .copied()
            .collect(),
        None => vec![("Esc", "back"), ("^R", "reload")],
    };
    frame.render_widget(
        Paragraph::new(Line::from(fitting_keys(&keys, key_row.width))).style(t.diff.header),
        key_row,
    );
    if let Some(note) = note {
        let color = if note.starts_with("sent ") {
            t.success
        } else {
            t.warning
        };
        frame.render_widget(
            Paragraph::new(format!(" {note}")).style(fg(color)),
            note_row,
        );
    }
    frame.render_widget(Paragraph::new(pr_tabs(pr, tab)).style(t.header), tabs_row);
    // A row under the tabs, and a column of margin at the right.
    let content = Rect {
        y: content.y + content.height.min(1),
        height: content.height.saturating_sub(1),
        width: content.width.saturating_sub(1),
        ..content
    };
    let Some(pr) = pr else {
        let branch = tree
            .and_then(|t| t.branch.as_deref())
            .unwrap_or("this branch");
        let lines = match answer {
            None => vec![Line::raw("reading the PR...")],
            Some(Err(why)) => vec![
                Line::styled(why.clone(), fg(t.danger)),
                Line::styled("^R reads it again", fg(t.muted)),
            ],
            Some(Ok(_)) => vec![
                Line::raw(format!("No pull request for {branch} yet.")),
                Line::styled(
                    "Esc, then s, starts a ship run, which pushes the branch and opens one.",
                    fg(t.muted),
                ),
            ],
        };
        // Inset a column, so a wrapped note keeps its margin.
        let content = Rect {
            x: content.x + content.width.min(1),
            width: content.width.saturating_sub(1),
            ..content
        };
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), content);
        return;
    };
    let width = usize::from(content.width);
    match tab {
        Tab::Overview => {
            // Inset a column, so a wrapped line keeps the indent its first
            // row has rather than running into the border.
            let content = Rect {
                x: content.x + content.width.min(1),
                width: content.width.saturating_sub(1),
                ..content
            };
            let lines = pr_overview(pr, width.saturating_sub(1));
            let most = lines.len().saturating_sub(usize::from(content.height) / 2);
            let scroll = u16::try_from(popup.selected().min(most)).unwrap_or(u16::MAX);
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .scroll((scroll, 0)),
                content,
            );
        }
        Tab::Checks => draw_pr_checks(frame, pr, popup.selected(), content),
        Tab::Comments => {
            let (lines, at) = pr_threads(pr, popup.selected(), width);
            draw_scrolled(frame, lines, at, popup.offset, content);
        }
        Tab::CodeRabbit => {
            let (lines, at) = pr_findings(pr, popup, width);
            draw_scrolled(frame, lines, at, popup.offset, content);
        }
    }
}

/// The tab row: each tab with what it holds, the one on show as a badge.
fn pr_tabs(pr: Option<&PrView>, on: Tab) -> Line<'static> {
    let t = theme();
    let Some(pr) = pr else {
        return Line::from(Span::styled(" 1 Overview ", t.badge));
    };
    let mut spans = Vec::new();
    for (i, tab) in pr_view::tabs(pr).into_iter().enumerate() {
        let (name, count, color) = match tab {
            Tab::Overview => ("Overview", String::new(), t.text),
            Tab::Checks => {
                let failed = count_checks(pr, CheckOutcome::Failed);
                let count = if failed > 0 {
                    format!("✗{failed}")
                } else {
                    format!("{}", pr.checks.len())
                };
                ("Checks", count, if failed > 0 { t.danger } else { t.text })
            }
            Tab::Comments => {
                let open = pr.threads.iter().filter(|t| !t.resolved).count();
                ("Comments", open.to_string(), t.warning)
            }
            Tab::CodeRabbit => {
                let reviewing = pr.bot.as_ref().is_some_and(|b| b.reviewing);
                let open = pr_view::actionable(pr).len();
                let count = if reviewing {
                    "…".to_string()
                } else {
                    open.to_string()
                };
                ("CodeRabbit", count, t.info)
            }
        };
        let style = if tab == on { t.badge } else { t.header };
        spans.push(Span::styled(format!(" {} {name} ", i + 1), style));
        if !count.is_empty() {
            let count_style = if tab == on { style } else { style.fg(color) };
            spans.push(Span::styled(format!("{count} "), count_style));
        }
        spans.push(Span::styled(" ", t.header));
    }
    Line::from(spans)
}

/// `1 issue`, `2 issues`.
fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// `line` padded with its own style to `width`, so a selection or a code
/// line's colour runs to the popup's edge.
fn full(line: Line<'static>, width: usize) -> Line<'static> {
    let room = width.saturating_sub(line.width());
    let mut line = line;
    line.spans.push(Span::raw(" ".repeat(room)));
    line
}

fn count_checks(pr: &PrView, outcome: CheckOutcome) -> usize {
    pr.checks.iter().filter(|c| c.outcome == outcome).count()
}

/// The Overview: who wants to merge what, where its checks, review,
/// CodeRabbit, threads and merge stand, then its description.
fn pr_overview(pr: &PrView, width: usize) -> Vec<Line<'static>> {
    let t = theme();
    let muted = fg(t.muted);
    let mut lines = vec![
        Line::styled(pr.title.clone(), fg(t.title).add_modifier(Modifier::BOLD)),
        Line::from(vec![
            Span::styled(
                format!(
                    "{} {} {} into {} from {} · ",
                    pr.author,
                    match pr.state.as_str() {
                        "MERGED" => "merged",
                        "CLOSED" => "wanted to merge",
                        _ => "wants to merge",
                    },
                    plural(usize::try_from(pr.commits).unwrap_or(usize::MAX), "commit"),
                    pr.base,
                    pr.branch
                ),
                muted,
            ),
            Span::styled(format!("+{}", pr.additions), fg(t.success)),
            Span::raw(" "),
            Span::styled(format!("-{}", pr.deletions), fg(t.danger)),
            Span::styled(
                format!(
                    " in {}",
                    plural(usize::try_from(pr.files).unwrap_or(usize::MAX), "file")
                ),
                muted,
            ),
        ]),
        Line::raw(""),
    ];
    // A row wraps under its value, not under its label.
    let row = |label: &str, parts: Vec<Span<'static>>| {
        let mut spans = vec![Span::styled(format!("{label:<12}"), muted)];
        spans.extend(parts);
        wrap_spans(spans, width, 12)
    };
    let (failed, passed, running) = (
        count_checks(pr, CheckOutcome::Failed),
        count_checks(pr, CheckOutcome::Passed),
        count_checks(pr, CheckOutcome::Pending),
    );
    let mut checks = Vec::new();
    if failed > 0 {
        let names: Vec<&str> = pr
            .checks
            .iter()
            .filter(|c| c.outcome == CheckOutcome::Failed)
            .map(|c| c.name.as_str())
            .collect();
        checks.push(Span::styled(
            format!("✗ {failed} failed: {}   ", names.join(", ")),
            fg(t.danger),
        ));
    }
    if running > 0 {
        checks.push(Span::styled(
            format!("● {running} running   "),
            fg(t.warning),
        ));
    }
    if passed > 0 {
        checks.push(Span::styled(format!("✓ {passed} passed   "), fg(t.success)));
    }
    let cancelled = count_checks(pr, CheckOutcome::Cancelled);
    if cancelled > 0 {
        checks.push(Span::styled(format!("⊘ {cancelled} cancelled"), muted));
    }
    if checks.is_empty() {
        checks.push(Span::styled("none", muted));
    }
    lines.extend(row("Checks", checks));
    let review = match pr.review.as_str() {
        "" => Span::styled("none yet", muted),
        "approved" => Span::styled("approved", fg(t.success)),
        "changes requested" => Span::styled("changes requested", fg(t.danger)),
        other => Span::styled(other.to_string(), fg(t.warning)),
    };
    lines.extend(row("Review", vec![review]));
    if let Some(bot) = &pr.bot {
        let count = |kind| {
            bot.findings
                .iter()
                .filter(|f| f.kind == kind && !f.resolved)
                .count()
        };
        let mut parts = vec![
            Span::styled(
                plural(count(coderabbit::Kind::Issue), "issue"),
                fg(t.danger),
            ),
            Span::raw(format!(
                ", {}, {}",
                plural(count(coderabbit::Kind::Suggestion), "suggestion"),
                plural(count(coderabbit::Kind::Nitpick), "nitpick")
            )),
        ];
        // Named as the tab names them; comments only where there are some.
        let comments = count(coderabbit::Kind::Comment);
        if comments > 0 {
            parts.push(Span::raw(format!(", {}", plural(comments, "comment"))));
        }
        if bot.reviewing {
            let note = if bot.reviewed.as_deref() == Some(pr.head.as_str()) {
                "   reviewing it again"
            } else {
                "   reviewing the newest push"
            };
            parts.push(Span::styled(note, fg(t.warning)));
        }
        lines.extend(row("CodeRabbit", parts));
    }
    // What the Comments tab counts: the open threads and the comments on
    // the whole PR, beside the resolved ones.
    let on_lines = pr.threads.iter().filter(|t| t.path.is_some());
    let open = on_lines.clone().filter(|t| !t.resolved).count();
    let resolved = on_lines.filter(|t| t.resolved).count();
    let general = pr.threads.iter().filter(|t| t.path.is_none()).count();
    lines.extend(row(
        "Comments",
        vec![
            Span::styled(
                format!("{open} open"),
                fg(if open > 0 { t.warning } else { t.text }),
            ),
            Span::raw(format!(", {resolved} resolved, {general} on the PR")),
        ],
    ));
    if let Some(why) = &pr.threads_error {
        lines.extend(row(
            "",
            vec![Span::styled(
                format!("review threads could not be read: {why}"),
                fg(t.danger),
            )],
        ));
    }
    let merge = match (pr.state.as_str(), pr.merge.as_str()) {
        ("MERGED", _) => Span::styled("merged", fg(t.special)),
        ("CLOSED", _) => Span::styled("closed", muted),
        _ if pr.draft => Span::styled("a draft, not ready for review", fg(t.warning)),
        (_, "clean" | "has_hooks") => Span::styled("ready to merge", fg(t.success)),
        (_, "blocked") => Span::styled("blocked", fg(t.danger)),
        (_, "dirty") => Span::styled("conflicts with its base", fg(t.danger)),
        (_, "behind") => Span::styled("behind its base", fg(t.warning)),
        // GitHub calls a PR unstable while a check is running, as well as
        // once one has failed.
        (_, "unstable") if failed > 0 => Span::styled("checks failing", fg(t.warning)),
        (_, "unstable") if running > 0 => Span::styled("waiting on checks", fg(t.warning)),
        (_, "unstable") => Span::styled("unstable", fg(t.warning)),
        (_, "draft") => Span::styled("draft", muted),
        _ => Span::styled("not known yet", muted),
    };
    lines.extend(row("Merge", vec![merge]));
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        format!("─ Description {}", "─".repeat(width.saturating_sub(15))),
        fg(t.border),
    ));
    if pr.body.trim().is_empty() {
        lines.push(Line::styled("No description.", muted));
    }
    for piece in issues::pieces(&tables_as_code(&pr.body), &palette()) {
        if let issues::Piece::Lines(body) = piece {
            for line in body {
                let (style, lead) = (line.style, continuation(&line));
                lines.extend(
                    wrap_led(line.spans, width, lead)
                        .into_iter()
                        .map(|l| l.style(style)),
                );
            }
        }
    }
    lines
}

/// The checks, one a row, and below them the selected one's log's end.
fn draw_pr_checks(frame: &mut Frame, pr: &PrView, selected: usize, area: Rect) {
    let t = theme();
    let muted = fg(t.muted);
    if pr.checks.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(" No checks on this PR.", muted)),
            area,
        );
        return;
    }
    // A name takes what the row has beside its mark, workflow and time.
    let name = usize::from(area.width)
        .saturating_sub(3 + 20 + 8 + 2)
        .max(28);
    let mut rows: Vec<Line> = Vec::new();
    for (i, check) in pr.checks.iter().enumerate() {
        let (mark, color) = check_mark(check.outcome);
        let time = check.seconds.map(duration).unwrap_or_default();
        let line = Line::from(vec![
            Span::styled(format!(" {mark} "), fg(color).add_modifier(Modifier::BOLD)),
            Span::raw(pad(&fit(&check.name, name - 1), name)),
            Span::styled(pad(&fit(&check.workflow, 19), 20), muted),
            Span::raw(format!("{time:>8}")),
        ]);
        rows.push(if i == selected {
            full(line.style(t.cursor), usize::from(area.width))
        } else {
            line
        });
    }
    let list_height = u16::try_from(rows.len())
        .unwrap_or(u16::MAX)
        .min(area.height / 2)
        .max(1);
    let [list, _, log_area] = Layout::vertical([
        Constraint::Length(list_height),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area);
    let skip = selected.saturating_sub(usize::from(list_height).saturating_sub(1));
    frame.render_widget(Paragraph::new(rows.split_off(skip.min(rows.len()))), list);
    let Some(check) = pr.checks.get(selected) else {
        return;
    };
    let room = usize::from(log_area.width).saturating_sub(2).max(1);
    // A line of the detail wrapped at words, so a long name keeps its end.
    let said = |text: &str, style: Style| -> Vec<Line<'static>> {
        word_wrap(text, room)
            .into_iter()
            .map(|row| Line::styled(format!(" {row}"), style))
            .collect()
    };
    let mut lines = Vec::new();
    match (check.outcome, check.log.as_deref()) {
        (CheckOutcome::Failed, log) => {
            lines.extend(said(
                &format!("─ {} · the end of its log ─", check.name),
                fg(t.border),
            ));
            let heading = lines.len();
            let unread = format!(
                "treetop reads the logs of the first {} failed checks; Enter opens this one on GitHub.",
                crate::ship::VIEW_LOGS
            );
            let log = log.unwrap_or(&unread);
            // Wrapped at words, each row keeping the indent its line had.
            let all: Vec<String> = log
                .lines()
                .flat_map(|l| {
                    let indent = l.len() - l.trim_start().len();
                    let (pad, text) = l.split_at(indent);
                    let rows = word_wrap(text, room.saturating_sub(indent).max(10));
                    let rows = if rows.is_empty() {
                        vec![String::new()]
                    } else {
                        rows
                    };
                    rows.into_iter().map(move |row| format!("{pad}{row}"))
                })
                .collect();
            // Under its heading, above the line naming the keys.
            let fits = usize::from(log_area.height)
                .saturating_sub(heading + 1)
                .max(1);
            let from = all.len().saturating_sub(fits);
            for text in &all[from..] {
                let text = text.as_str();
                let style = if text.contains("error") || text.contains("Error") {
                    fg(t.danger)
                } else {
                    Style::new()
                };
                lines.push(Line::styled(format!(" {text}"), style));
            }
            lines.push(Line::styled(
                " f sends this to the tree's agent · Enter opens the run on GitHub",
                muted,
            ));
        }
        (CheckOutcome::Pending, _) => {
            lines.extend(said(
                &format!("{} is still running.", check.name),
                fg(t.warning),
            ));
        }
        (outcome, _) => {
            let word = match outcome {
                CheckOutcome::Skipped => "was skipped",
                CheckOutcome::Cancelled => "was cancelled",
                _ => "passed",
            };
            let time = check
                .seconds
                .filter(|_| outcome != CheckOutcome::Cancelled)
                .map(|s| format!(" in {}", duration(s)))
                .unwrap_or_default();
            lines.extend(said(
                &format!("{} {word}{time}.", check.name),
                fg(t.success),
            ));
            lines.push(Line::styled(" Enter opens it on GitHub", muted));
        }
    }
    frame.render_widget(Paragraph::new(lines), log_area);
}

fn check_mark(outcome: CheckOutcome) -> (&'static str, Color) {
    let t = theme();
    match outcome {
        CheckOutcome::Passed => ("✓", t.success),
        CheckOutcome::Failed => ("✗", t.danger),
        CheckOutcome::Pending => ("●", t.warning),
        CheckOutcome::Skipped => ("-", t.muted),
        CheckOutcome::Cancelled => ("⊘", t.muted),
    }
}

/// `1m 12s`, or `40s` under a minute.
fn duration(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        _ => format!("{}m {:02}s", seconds / 60, seconds % 60),
    }
}

/// A place cut from its start to `width` columns, at a folder where it can,
/// so its file name and lines stay: `…/navigation/Nav.kt:1613-1620`.
fn tail_fit(place: &str, width: usize) -> String {
    let chars: Vec<char> = place.chars().collect();
    if chars.len() <= width {
        return place.to_string();
    }
    let tail: String = chars[chars.len() - width.saturating_sub(1)..]
        .iter()
        .collect();
    let tail = match tail.find('/') {
        Some(at) if at + 1 < tail.len() => &tail[at..],
        _ => tail.as_str(),
    };
    format!("…{tail}")
}

/// `lines` scrolled so the selected item, rows `at.0` to `at.1`, is on
/// screen, then `extra` rows on. An item taller than the screen starts at
/// its top, or at the tab's top where it is near it, so nothing above it is
/// lost; `d` and `u` scroll through the rest.
fn draw_scrolled(
    frame: &mut Frame,
    lines: Vec<Line<'static>>,
    at: (usize, usize),
    extra: usize,
    area: Rect,
) {
    let height = usize::from(area.height);
    let (start, end) = at;
    let mut base = end.saturating_sub(height);
    if start < base {
        base = if start < height / 2 { 0 } else { start };
    }
    let offset = (base + extra).min(lines.len().saturating_sub(height).max(base));
    let shown: Vec<Line> = lines.into_iter().skip(offset).collect();
    frame.render_widget(Paragraph::new(shown), area);
}

/// `spans` broken into rows of at most `width` columns at spaces, or within
/// a word longer than a row, each row after the first starting `hang`
/// columns in; the spans keep their styles.
fn wrap_spans(spans: Vec<Span<'static>>, width: usize, hang: usize) -> Vec<Line<'static>> {
    wrap_led(spans, width, vec![Span::raw(" ".repeat(hang))])
}

/// `spans` wrapped as `wrap_spans` does, each row after the first starting
/// with `lead`, such as a quote's bar: the bar runs down the whole quote.
fn wrap_led(
    spans: Vec<Span<'static>>,
    width: usize,
    lead: Vec<Span<'static>>,
) -> Vec<Line<'static>> {
    let hang: usize = lead.iter().map(Span::width).sum();
    let width = width.max(hang + 10);
    // Words run across spans, as a link's `(` and its styled text do, and
    // end at a space, which they keep.
    let mut words: Vec<Vec<(String, Style)>> = vec![Vec::new()];
    for span in &spans {
        for c in span.content.chars() {
            let word = words.last_mut().expect("words starts with one");
            match word.last_mut() {
                Some((text, style)) if *style == span.style => text.push(c),
                _ => word.push((c.to_string(), span.style)),
            }
            if c == ' ' {
                words.push(Vec::new());
            }
        }
    }
    // Columns as the terminal draws them: a CJK character takes two.
    let cols = |text: &str| unicode_width::UnicodeWidthStr::width(text);
    let size = |word: &[(String, Style)]| word.iter().map(|(t, _)| cols(t)).sum::<usize>();
    let bare = |word: &[(String, Style)]| {
        size(word) - word.last().map_or(0, |(t, _)| cols(t) - cols(t.trim_end()))
    };
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let (mut used, mut is_empty) = (0, true);
    for word in words.into_iter().filter(|w| !w.is_empty()) {
        // A word longer than a whole row is cut where it stands rather than
        // leaving the row before it, a bullet say, on its own.
        if !is_empty && used + bare(&word) > width && bare(&word) <= width - hang {
            rows.push(lead.clone());
            used = hang;
        }
        for (text, style) in word {
            let mut text = text;
            while used + cols(text.trim_end()) > width {
                let room = width.saturating_sub(used);
                // As many chars as fit the room, and at least one.
                let mut fill = 0;
                let cut = text
                    .char_indices()
                    .find(|(_, c)| {
                        fill += unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
                        fill > room
                    })
                    .map_or(text.len(), |(at, _)| {
                        at.max(text.chars().next().map_or(0, char::len_utf8))
                    });
                let head = text[..cut].to_string();
                text = text[cut..].to_string();
                if let Some(row) = rows.last_mut() {
                    row.push(Span::styled(head, style));
                }
                rows.push(lead.clone());
                used = hang;
            }
            used += cols(&text);
            if let Some(row) = rows.last_mut() {
                row.push(Span::styled(text, style));
            }
        }
        is_empty = false;
    }
    rows.into_iter().map(Line::from).collect()
}

/// A comment's Markdown drawn as the description is, without the HTML a
/// browser would hide, wrapped after `indent` columns.
fn markdown(text: &str, width: usize, indent: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let text = with_marks(&tables_as_code(&coderabbit::readable(text)));
    for piece in issues::pieces(&text, &palette()) {
        let issues::Piece::Lines(lines) = piece else {
            continue;
        };
        for line in lines {
            let style = line.style;
            let lead = continuation(&line);
            for row in wrap_led(line.spans, width.saturating_sub(indent + 1), lead) {
                let mut spans = vec![Span::raw(" ".repeat(indent))];
                spans.extend(row.spans);
                out.push(Line::from(spans).style(style));
            }
        }
    }
    out
}

/// `text` with each Markdown table fenced as code, so its rows keep their
/// lines and columns rather than running together as one paragraph.
fn tables_as_code(text: &str) -> String {
    let mut out = Vec::new();
    let mut in_table = false;
    for line in text.lines() {
        let is_row = line.trim_start().starts_with('|');
        if is_row != in_table {
            out.push("```".to_string());
            in_table = is_row;
        }
        out.push(if is_row {
            as_words(line)
        } else {
            line.to_string()
        });
    }
    if in_table {
        out.push("```".to_string());
    }
    out.join("\n")
}

/// A table row's links and pictures as their words, since a code block
/// shows Markdown as it is written.
fn as_words(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        let close = rest[open..].find("](").map(|c| open + c);
        let end = close.and_then(|c| rest[c..].find(')').map(|e| c + e));
        let (Some(close), Some(end)) = (close, end) else {
            break;
        };
        out.push_str(rest[..open].trim_end_matches('!'));
        out.push_str(&rest[open + 1..close]);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// GitHub's shortcodes for the marks bots use, as the marks.
fn with_marks(text: &str) -> String {
    [
        (":x:", "❌"),
        (":warning:", "⚠️"),
        (":white_check_mark:", "✅"),
        (":heavy_check_mark:", "✔️"),
    ]
    .iter()
    .fold(text.to_string(), |text, (code, mark)| {
        text.replace(code, mark)
    })
}

/// What a line's wrapped rows start with, so they continue under its text:
/// a quote's bars again, then spaces for any indent and a list item's
/// bullet (`• `, `- ` or `12. `).
fn continuation(line: &Line) -> Vec<Span<'static>> {
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let mut rest = text.as_str();
    let mut bars = 0;
    while let Some(after) = rest.strip_prefix("│ ") {
        bars += 1;
        rest = after;
    }
    let indent = rest.chars().take_while(|c| *c == ' ').count();
    let rest = &rest[indent..];
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    let marker = if ["• ", "- ", "* "].iter().any(|b| rest.starts_with(b)) {
        2
    } else if digits > 0 && rest[digits..].starts_with(". ") {
        digits + 2
    } else {
        0
    };
    let mut lead = Vec::new();
    if bars > 0 {
        lead.push(Span::styled("│ ".repeat(bars), fg(theme().muted)));
    }
    lead.push(Span::raw(" ".repeat(indent + marker)));
    lead
}

/// `text` padded with spaces to `width` columns as the terminal draws it,
/// so a wide character such as an emoji keeps later columns in line.
fn pad(text: &str, width: usize) -> String {
    let room = width.saturating_sub(Line::raw(text).width());
    format!("{text}{}", " ".repeat(room))
}

/// The first line of a comment as it reads, for a folded row.
fn first_line(text: &str) -> String {
    markdown(text, usize::MAX / 2, 0)
        .into_iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
}

/// Prose wrapped to `width` at words, each row after `indent`.
fn prose(text: &str, width: usize, indent: &str, style: Style) -> Vec<Line<'static>> {
    let room = width.saturating_sub(indent.chars().count() + 1).max(10);
    text.lines()
        .flat_map(|line| {
            if line.trim().is_empty() {
                vec![String::new()]
            } else {
                word_wrap(line, room)
            }
        })
        .map(|row| Line::styled(format!("{indent}{row}"), style))
        .collect()
}

/// Code lines from a diff hunk or a suggested change, each starting `+`,
/// `-` or a space, behind the diff's line colours and in its code colours.
fn code_lines(
    code: &[String],
    path: Option<&str>,
    last: Option<u64>,
    indent: &str,
    width: usize,
) -> Vec<Line<'static>> {
    let t = theme();
    let palette = &t.diff;
    let grammar = path.and_then(syntax::find);
    let mut highlighter = grammar.map(syntax::Highlighter::new);
    let shown = code.iter().filter(|l| !l.starts_with('-')).count();
    let mut number = last.map(|l| l + 1 - shown as u64);
    code.iter()
        .flat_map(|line| {
            let (sign, text) = line.split_at(line.len().min(1));
            // As the diff draws a tab, so every row is as wide as it says.
            let text = text.replace('\t', "    ");
            let text = text.as_str();
            let bg = match sign {
                "+" => Some(palette.added),
                "-" => Some(palette.removed),
                _ => None,
            };
            let label = match (sign, number) {
                ("-", _) | (_, None) => "    ".to_string(),
                (_, Some(n)) => {
                    number = Some(n + 1);
                    format!("{n:>4}")
                }
            };
            let gutter = format!("{indent}{label} {sign} ");
            let hang = gutter.chars().count();
            let mut spans = vec![Span::styled(gutter, fg(palette.number))];
            let tokens = highlighter
                .as_mut()
                .map(|h| h.line(text))
                .unwrap_or_default();
            spans.extend(coloured(text, &tokens));
            // A long line wraps under its own code, behind its colour.
            wrap_spans(spans, width.saturating_sub(1), hang)
                .into_iter()
                .map(|row| match bg {
                    Some(bg) => full(row.style(Style::new().bg(bg)), width),
                    None => row,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// `text` in its tokens' colours, where the theme colours code.
fn coloured(text: &str, tokens: &[(std::ops::Range<usize>, syntax::Token)]) -> Vec<Span<'static>> {
    let Some(syntax) = theme().diff.syntax.as_ref() else {
        return vec![Span::raw(text.to_string())];
    };
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut at = 0;
    for (range, token) in tokens {
        let (start, end) = (range.start.min(chars.len()), range.end.min(chars.len()));
        if start > at {
            spans.push(Span::raw(chars[at..start].iter().collect::<String>()));
        }
        if end > start {
            spans.push(Span::styled(
                chars[start..end].iter().collect::<String>(),
                syntax.style(*token),
            ));
        }
        at = at.max(end);
    }
    if at < chars.len() {
        spans.push(Span::raw(chars[at..].iter().collect::<String>()));
    }
    spans
}

/// The Comments tab's lines, and the rows the selected thread takes.
fn pr_threads(pr: &PrView, selected: usize, width: usize) -> (Vec<Line<'static>>, (usize, usize)) {
    let t = theme();
    let muted = fg(t.muted);
    let mut lines = Vec::new();
    let mut at = (0, 0);
    if let Some(why) = &pr.threads_error {
        lines.push(Line::styled(
            format!(" review threads could not be read: {why}"),
            fg(t.danger),
        ));
        lines.push(Line::raw(""));
    } else if pr.threads.is_empty() {
        lines.push(Line::styled(" No review comments yet.", muted));
    }
    for (i, thread) in pr.threads.iter().enumerate() {
        let start = lines.len();
        let is_on = i == selected;
        let (mark, color) = if thread.resolved {
            ("✓", t.success)
        } else {
            ("●", t.warning)
        };
        // Its code is numbered from where the comment was written, so the
        // header names that line, and where it is now when it has moved.
        let shows_code = !thread.code.is_empty() && !(thread.resolved && !is_on);
        let lines_text = match (thread.line, thread.code_line) {
            (Some(now), Some(then)) if shows_code && now != then => {
                Some(format!("{then} (now {now})"))
            }
            (now, _) => now.map(|l| l.to_string()),
        };
        let place = pr_view::place(thread.path.as_deref(), lines_text.as_deref());
        let place = if place.is_empty() {
            " commented".to_string()
        } else {
            // What the row has after its mark, author and, folded, its note.
            let note = if thread.resolved && !is_on { 12 } else { 0 };
            let room = width.saturating_sub(thread.author.chars().count() + 8 + note);
            format!(" on {}", tail_fit(&place, room.min(width / 2)))
        };
        let mut header = vec![
            Span::styled(format!(" {mark} "), fg(color).add_modifier(Modifier::BOLD)),
            Span::styled(
                thread.author.clone(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::styled(place, muted),
        ];
        // Folded, a resolved thread is its header and the start of what
        // was said, on one row.
        let is_folded = thread.resolved && !is_on;
        if is_folded {
            // "resolved" stays whole; the start of what was said gives way.
            let first = first_line(&thread.body);
            let used: usize = header.iter().map(Span::width).sum::<usize>() + 11;
            header.push(Span::styled(" · resolved", muted));
            if used + 4 < width {
                header.push(Span::styled(
                    fit(&format!(" · {first}"), width.saturating_sub(used + 1)),
                    muted,
                ));
            }
        }
        let header = Line::from(header);
        lines.push(if is_on {
            full(header.style(t.cursor), width)
        } else {
            header
        });
        if !is_folded {
            lines.extend(code_lines(
                &thread.code,
                thread.path.as_deref(),
                thread.code_line,
                "   ",
                width,
            ));
            lines.extend(markdown(&thread.body, width, 3));
            for reply in &thread.replies {
                lines.push(Line::from(vec![
                    Span::styled("   ↳ ", muted),
                    Span::styled(
                        reply.author.clone(),
                        Style::new().add_modifier(Modifier::BOLD),
                    ),
                ]));
                lines.extend(markdown(&reply.body, width, 5));
            }
        }
        if is_on {
            at = (start, lines.len());
        }
        lines.push(Line::raw(""));
    }
    (lines, at)
}

/// The CodeRabbit tab's lines, and the rows the selected finding takes.
fn pr_findings(pr: &PrView, popup: &PrPopup, width: usize) -> (Vec<Line<'static>>, (usize, usize)) {
    let t = theme();
    let muted = fg(t.muted);
    let mut lines = Vec::new();
    let mut at = (0, 0);
    let Some(bot) = &pr.bot else {
        return (lines, at);
    };
    let reviewed = bot.reviewed.as_deref().map(|sha| &sha[..sha.len().min(7)]);
    let first = bot.reviewing && reviewed.is_none();
    if first {
        // Nothing yet to show or to say it is from before.
        lines.push(Line::styled(
            " CodeRabbit is reviewing this PR for the first time.",
            fg(t.warning),
        ));
    } else {
        let whose = match (reviewed, bot.reviewed.as_deref() == Some(pr.head.as_str())) {
            (Some(sha), true) => format!(" coderabbitai reviewed {sha}, the head"),
            (Some(sha), false) => format!(" coderabbitai reviewed {sha}, an earlier commit"),
            (None, _) => " coderabbitai has not reviewed this PR yet".to_string(),
        };
        lines.push(Line::styled(whose, muted));
    }
    if bot.reviewing && !first {
        let note = if bot.reviewed.as_deref() == Some(pr.head.as_str()) {
            " CodeRabbit is reviewing it again; these are from its last review."
        } else {
            " CodeRabbit is reviewing the newest push; these are from before it."
        };
        lines.push(Line::styled(note, fg(t.warning)));
    }
    lines.push(Line::raw(""));
    let findings = pr_view::findings(pr, popup.nitpicks);
    if findings.is_empty() && !first {
        lines.push(Line::styled(" CodeRabbit has no comments to show.", muted));
    }
    for (i, finding) in findings.iter().enumerate() {
        let start = lines.len();
        let is_on = i == popup.selected();
        let (tag, color) = match finding.kind {
            coderabbit::Kind::Issue => ("issue", t.danger),
            coderabbit::Kind::Suggestion => ("suggestion", t.info),
            coderabbit::Kind::Nitpick => ("nitpick", t.muted),
            coderabbit::Kind::Comment => ("comment", t.text),
        };
        let place = pr_view::place(finding.path.as_deref(), finding.lines.as_deref());
        let mut header = vec![
            Span::styled(
                format!(" {} ", if finding.resolved { "✓" } else { " " }),
                fg(t.success),
            ),
            Span::styled(format!("{tag:<11}"), fg(color).add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("{:<9}", finding.severity.as_deref().unwrap_or("")),
                muted,
            ),
            Span::styled(
                format!("{}  ", tail_fit(&place, width / 2)),
                fg(t.diff.path),
            ),
        ];
        let room = width.saturating_sub(header.iter().map(Span::width).sum::<usize>() + 1);
        header.push(Span::styled(
            fit(&finding.title.replace('`', ""), room),
            Style::new().add_modifier(Modifier::BOLD),
        ));
        if finding.resolved {
            header = header
                .into_iter()
                .map(|s| s.patch_style(Style::new().add_modifier(Modifier::DIM)))
                .collect();
        }
        let header = Line::from(header);
        lines.push(if is_on {
            full(header.style(t.cursor), width)
        } else {
            header
        });
        if is_on {
            // The header cuts a long title; the selected one shows it whole.
            if Line::raw(finding.title.as_str()).width() > room {
                lines.extend(prose(
                    &finding.title.replace('`', ""),
                    width,
                    "   ",
                    Style::new().add_modifier(Modifier::BOLD),
                ));
            }
            lines.extend(markdown(&finding.body, width, 3));
            if !finding.fix.is_empty() {
                lines.push(Line::styled("   Suggested change", muted));
                lines.extend(code_lines(
                    &finding.fix,
                    finding.path.as_deref(),
                    None,
                    "   ",
                    width,
                ));
            }
            if finding.prompt.is_some() {
                lines.push(Line::styled(
                    "   f sends CodeRabbit's own prompt for agents",
                    muted,
                ));
            }
            at = (start, lines.len());
            lines.push(Line::raw(""));
        }
    }
    let nitpicks = pr_view::nitpick_count(pr);
    if !popup.nitpicks && nitpicks > 0 {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!(" ▸ {}   z shows them", plural(nitpicks, "nitpick")),
            muted,
        ));
    }
    (lines, at)
}

/// An agent's chat in a popup over treetop, under a bar naming its branch,
/// tree and agent, with the keys out at the right of the same line. The
/// branch comes from the latest listing, so it follows the agent if it
/// switches branches.
fn draw_chat(frame: &mut Frame, app: &App, chat: &Chat) {
    let agent = agent_of(app, Some(chat.command()), &chat.agent, &chat.tree);
    let body = chat_frame(frame, app, &chat.agent, &chat.tree, agent);
    if !chat.is_ready() {
        draw_note(frame, body, &opening_note(&chat.agent));
        return;
    }
    if let Some(cursor) = draw_screen(chat.screen(), frame.buffer_mut(), body) {
        frame.set_cursor_position(cursor);
    }
}

/// The popup a chat on its way stands in with: the frame the chat opens in,
/// so the hand-over from the compose box to the chat never leaves the popup.
fn draw_opening(frame: &mut Frame, app: &App, opening: &Opening) {
    let agent = agent_of(app, None, &opening.agent, &opening.tree);
    let body = chat_frame(frame, app, &opening.agent, &opening.tree, agent);
    let note = if opening.tree == "new" {
        "leasing a tree for the agent...".to_string()
    } else {
        opening_note(&opening.agent)
    };
    draw_note(frame, body, &note);
}

fn opening_note(agent: &str) -> String {
    if assistant::is_assistant(agent) {
        "opening the assistant...".into()
    } else {
        format!("opening {agent}...")
    }
}

/// A chat's popup: its frame and bar, cleared, with the keys out at the
/// right of the bar, and its sidebar where there is room for one. Returns
/// where the chat's screen goes.
fn chat_frame(frame: &mut Frame, app: &App, agent: &str, tree: &str, seen: Option<&Agent>) -> Rect {
    let t = theme();
    let title = if assistant::is_assistant(agent) {
        vec![Span::styled(" assistant ", t.badge)]
    } else if tree == SCRATCH {
        vec![
            Span::styled(" scratch ", t.badge),
            Span::styled(
                format!(" {agent} "),
                Style::new().add_modifier(Modifier::BOLD),
            ),
        ]
    } else {
        let branch = app
            .trees
            .iter()
            .find(|candidate| candidate.name == tree)
            .and_then(|candidate| candidate.branch.clone())
            .unwrap_or_else(|| "(detached)".into());
        vec![
            Span::styled(" chat ", t.badge),
            Span::styled(
                format!(" {branch}"),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("  tree {tree}  {agent} ")),
        ]
    };
    let keys = key_bar(&[("^Q", "back to treetop"), ("^\\", "next chat")]);
    let screen = stage(app, frame.area());
    bar_popup(frame, screen, title, keys);
    if let Some(side) = popup::sidebar(screen) {
        let block = Block::new()
            .borders(Borders::LEFT)
            .border_style(fg(theme().border))
            .padding(Padding::horizontal(1));
        let inner = block.inner(side);
        frame.render_widget(block, side);
        let tree = app.trees.iter().find(|t| t.name == tree);
        let now = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let lines = sidebar_lines(seen, tree, app.usage.as_ref(), now, inner.width);
        frame.render_widget(Paragraph::new(lines), inner);
    }
    popup::chat(screen)
}

/// The chat's agent: the one its command attaches to, else the one of its
/// name in its tree.
fn agent_of<'a>(
    app: &'a App,
    command: Option<&[String]>,
    agent: &str,
    tree: &str,
) -> Option<&'a Agent> {
    let agents = || app.trees.iter().flat_map(|t| &t.agents);
    command
        .and_then(|command| agents().find(|a| a.attach.as_deref() == Some(command)))
        .or_else(|| {
            app.trees
                .iter()
                .filter(|t| t.name == tree)
                .flat_map(|t| &t.agents)
                .find(|a| a.name == agent)
        })
}

/// A span of `seconds` in a few characters: `45s`, `12m`, `3h`, `6d`.
fn span(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// A count of tokens in a few characters: `850`, `97k`, `1M`.
fn tokens(count: u64) -> String {
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => format!("{}k", count / 1_000),
        _ if count.is_multiple_of(1_000_000) => format!("{}M", count / 1_000_000),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}

fn percent(context: &Context) -> u64 {
    (context.used * 100)
        .checked_div(context.window)
        .unwrap_or(0)
}

/// How a context reads: loud as it nears the end of its window, where the
/// agent compacts it and forgets the detail.
fn context_style(context: &Context) -> Style {
    let t = theme();
    match percent(context) {
        90.. => fg(t.danger).add_modifier(Modifier::BOLD),
        75.. => fg(t.warning),
        _ => fg(t.muted),
    }
}

/// A bar `width` cells long, filled for `share` of 100.
fn meter(share: u64, width: u16) -> (String, String) {
    let width = usize::from(width);
    let filled = (usize::try_from(share.min(100)).unwrap_or(100) * width).div_ceil(100);
    ("█".repeat(filled), "░".repeat(width - filled))
}

/// The chat's sidebar: how full its agent's context is and where that comes
/// from; then how much of the plan is used and what this session has spent;
/// then where its tree stands. `now` is seconds since the epoch.
fn sidebar_lines(
    agent: Option<&Agent>,
    tree: Option<&Tree>,
    usage: Option<&Usage>,
    now: u64,
    width: u16,
) -> Vec<Line<'static>> {
    let context = agent.and_then(|a| a.context.as_ref());
    let mut lines = context_lines(context, width);
    // Nearing its compaction, an agent in a pool tree can hand its work to
    // a fresh one instead.
    if context.is_some_and(|c| percent(c) >= 75) && tree.is_some_and(|t| t.is_held() && !t.is_main)
    {
        lines.push(Line::styled(
            "Ctrl+Q, then h hands off",
            fg(theme().warning),
        ));
    }
    lines.push(Line::raw(""));
    lines.extend(usage_lines(usage, agent.and_then(|a| a.spend), now, width));
    if let Some(tree) = tree {
        lines.push(Line::raw(""));
        lines.extend(tree_lines(tree, width));
    }
    lines
}

fn heading(text: &str) -> Line<'static> {
    Line::styled(
        text.to_string(),
        fg(theme().title).add_modifier(Modifier::BOLD),
    )
}

/// How full the agent's context is, and an estimate of where it comes from,
/// each part with its share of what is used.
fn context_lines(context: Option<&Context>, width: u16) -> Vec<Line<'static>> {
    let t = theme();
    let mut lines = vec![heading("Context")];
    let Some(context) = context else {
        lines.push(Line::styled("not known for this agent", fg(t.muted)));
        return lines;
    };
    let share = percent(context);
    let (filled, empty) = meter(share, width.saturating_sub(5));
    lines.extend([
        Line::from(vec![
            Span::raw(tokens(context.used)),
            Span::styled(format!(" of {}", tokens(context.window)), fg(t.muted)),
        ]),
        Line::from(vec![
            Span::styled(filled, context_style(context)),
            Span::styled(empty, fg(t.border)),
            Span::styled(format!("{share:>4}%"), context_style(context)),
        ]),
    ]);
    if context.parts.is_empty() {
        return lines;
    }
    lines.extend([Line::raw(""), Line::styled("Estimated split", fg(t.muted))]);
    let name_width = usize::from(width).saturating_sub(11);
    lines.extend(context.parts.iter().map(|part| {
        let of = match (part.tokens * 100).checked_div(context.used).unwrap_or(0) {
            0 => "<1%".to_string(),
            of => format!("{of}%"),
        };
        Line::from(vec![
            Span::raw(format!("{:<name_width$}", part.name)),
            Span::raw(format!("{:>5}", tokens(part.tokens))),
            Span::styled(format!("{of:>6}"), fg(t.muted)),
        ])
    }));
    lines
}

/// How a limit reads: loud as it nears its end, where the agents stop.
fn limit_style(percent: u64) -> Style {
    let t = theme();
    match percent {
        90.. => fg(t.danger).add_modifier(Modifier::BOLD),
        75.. => fg(t.warning),
        _ => fg(t.muted),
    }
}

/// How much of the plan is used, a meter per limit with when it resets, and
/// how old that reading is; then the tokens this session has spent.
fn usage_lines(
    usage: Option<&Usage>,
    spend: Option<Spend>,
    now: u64,
    width: u16,
) -> Vec<Line<'static>> {
    let t = theme();
    let mut title = vec![Span::styled(
        "Usage",
        fg(t.title).add_modifier(Modifier::BOLD),
    )];
    if let Some(at) = usage.and_then(|u| u.at) {
        title.push(Span::styled(
            format!("  as of {} ago", span(now.saturating_sub(at))),
            fg(t.muted),
        ));
    }
    let mut lines = vec![Line::from(title)];
    match usage {
        None => lines.push(Line::styled("plan not known", fg(t.muted))),
        Some(usage) => {
            // The names in a column as wide as the longest, leaving the
            // meter at least half the row.
            let longest = usage.limits.iter().map(|l| l.name.chars().count()).max();
            let name_width = longest.unwrap_or(0).min(usize::from(width) / 2 - 1) + 1;
            let meter_width = width.saturating_sub(5 + u16::try_from(name_width).unwrap_or(0));
            for limit in &usage.limits {
                let (filled, empty) = meter(limit.percent, meter_width);
                let name: String = limit.name.chars().take(name_width - 1).collect();
                lines.push(Line::from(vec![
                    Span::raw(format!("{name:<name_width$}")),
                    Span::styled(filled, limit_style(limit.percent)),
                    Span::styled(empty, fg(t.border)),
                    Span::styled(format!("{:>4}%", limit.percent), limit_style(limit.percent)),
                ]));
                if let Some(at) = limit.resets_at.filter(|at| *at > now) {
                    lines.push(Line::styled(
                        format!("{:name_width$}resets in {}", "", span(at - now)),
                        fg(t.muted),
                    ));
                }
            }
        }
    }
    lines.extend([Line::raw(""), Line::styled("This session", fg(t.muted))]);
    let Some(spend) = spend else {
        lines.push(Line::styled("not known for this agent", fg(t.muted)));
        return lines;
    };
    let name_width = usize::from(width).saturating_sub(6);
    lines.extend(
        [
            ("input", spend.input),
            ("cache read", spend.cache_read),
            ("cache write", spend.cache_write),
            ("output", spend.output),
        ]
        .into_iter()
        .map(|(name, count)| {
            Line::from(vec![
                Span::raw(format!("{name:<name_width$}")),
                Span::raw(format!("{:>6}", tokens(count))),
            ])
        }),
    );
    lines
}

/// Where the chat's tree stands: its git state and PR as the list's columns
/// show them, then the ports it serves, then its PR's link and what its
/// plugin says about it.
fn tree_lines(tree: &Tree, width: u16) -> Vec<Line<'static>> {
    let t = theme();
    let width = usize::from(width);
    let mut lines = vec![heading(&format!("Tree {}", tree.name))];
    let mut state = match tree.work {
        None => vec![Span::styled("─", fg(t.warning))],
        Some(work) if work.changed + work.unpushed == 0 => {
            vec![Span::styled("clean", fg(t.success))]
        }
        Some(work) => git_spans(work),
    };
    if let Some(ship) = &tree.ship {
        state.push(Span::raw("  "));
        state.push(Span::styled(ship.label.clone(), stage_style(ship.stage)));
    }
    lines.extend([Line::from(state), Line::raw(""), heading("Ports")]);
    let served = tree.served();
    if served.is_empty() {
        lines.push(Line::styled("none", fg(t.muted)));
    }
    for (port, name) in served {
        let address = format!("localhost:{port}");
        let room = width.saturating_sub(address.chars().count() + 1);
        lines.push(Line::from(vec![
            Span::styled(address, fg(t.info)),
            Span::styled(
                format!(" {}", name.chars().take(room).collect::<String>()),
                fg(t.muted),
            ),
        ]));
    }
    lines.extend([Line::raw(""), heading("PR")]);
    let Some(ship) = &tree.ship else {
        lines.push(Line::styled("none", fg(t.muted)));
        return lines;
    };
    if let Some(url) = &ship.url {
        let url = url.strip_prefix("https://").unwrap_or(url);
        lines.extend(
            wrap_chars(url, width)
                .into_iter()
                .filter(|row| !row.is_empty())
                .map(|row| Line::styled(row, fg(t.info))),
        );
    }
    for detail in &ship.detail {
        lines.extend(
            word_wrap(detail, width)
                .into_iter()
                .map(|row| Line::styled(row, fg(t.muted))),
        );
    }
    lines
}

/// A popup with a bar on the first row inside its border, drawn like a
/// diff's file header: `title` at its left and `keys` at its right, the keys
/// left out where the two would not fit. Returns the rows under the bar.
fn bar_popup(
    frame: &mut Frame,
    screen: Rect,
    title: Vec<Span<'static>>,
    keys: Vec<Span<'static>>,
) -> Rect {
    let area = popup::area(screen);
    let bar = popup::bar(screen);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::bordered()
            .border_style(fg(theme().accent))
            .style(fg(theme().text)),
        area,
    );
    let title = Line::from(title);
    let keys = Line::from(keys);
    let fits = title.width() + keys.width() <= usize::from(bar.width);
    frame.render_widget(Paragraph::new(title).style(theme().diff.header), bar);
    if fits {
        frame.render_widget(Paragraph::new(keys.right_aligned()), bar);
    }
    popup::body(screen)
}

/// One muted line in the middle of `area`.
fn draw_note(frame: &mut Frame, area: Rect, note: &str) {
    let [_, middle, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(note.to_string())
            .style(fg(theme().muted))
            .centered(),
        middle,
    );
}

fn terminal_color(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(index) => Color::Indexed(index),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Copies a terminal screen into `area` of the buffer, cell by cell, with
/// its colours and attributes. Returns where the cursor goes, None when the
/// program has hidden it or it lies outside the area.
fn draw_screen(screen: &vt100::Screen, buffer: &mut Buffer, area: Rect) -> Option<Position> {
    for row in 0..area.height {
        for col in 0..area.width {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let mut style = Style::new()
                .fg(terminal_color(cell.fgcolor()))
                .bg(terminal_color(cell.bgcolor()))
                .remove_modifier(Modifier::all());
            for (is_on, modifier) in [
                (cell.bold(), Modifier::BOLD),
                (cell.dim(), Modifier::DIM),
                (cell.italic(), Modifier::ITALIC),
                (cell.underline(), Modifier::UNDERLINED),
                (cell.inverse(), Modifier::REVERSED),
            ] {
                if is_on {
                    style = style.add_modifier(modifier);
                }
            }
            let symbol = if cell.has_contents() {
                cell.contents()
            } else {
                " "
            };
            if let Some(target) = buffer.cell_mut((area.x + col, area.y + row)) {
                target.set_symbol(symbol).set_style(style);
            }
        }
    }
    let (row, col) = screen.cursor_position();
    (!screen.hide_cursor() && row < area.height && col < area.width)
        .then(|| Position::new(area.x + col, area.y + row))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_words_and_cuts_a_word_too_long_for_a_row() {
        assert_eq!(
            word_wrap("tell the agent what", 9),
            ["tell the", "agent", "what"]
        );
        assert_eq!(word_wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
    }

    /// The `?` overlay puts its groups in four columns where a laptop's
    /// terminal has room, two where it has less and one on a phone's width,
    /// never cutting a line or dropping a key.
    #[test]
    fn help_goes_four_columns_wide_then_two_then_one() {
        let text = |line: &Line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        let keys: usize = HELP.iter().map(|(_, keys)| keys.len()).sum();
        let mut last_rows = 0;
        for (room, headings) in [
            (130, vec!["Agents", "Trees", "Ports", "Issues"]),
            (80, vec!["Agents", "Ports"]),
            (40, vec!["Agents"]),
        ] {
            let (lines, width) = help_layout(&HELP, room);
            assert!(width <= room, "{width} columns fit in {room}");
            let first = text(&lines[0]);
            let found: Vec<&str> = first.split_whitespace().collect();
            assert_eq!(found, headings, "at {room} columns");
            let described: usize = HELP
                .iter()
                .flat_map(|(_, keys)| keys.iter())
                .filter(|(_, does)| lines.iter().any(|line| text(line).contains(does)))
                .count();
            assert_eq!(described, keys, "every key at {room} columns");
            assert!(lines.len() > last_rows, "fewer columns, taller box");
            last_rows = lines.len();
        }
    }

    #[test]
    fn a_shared_port_carries_the_key_that_shared_it() {
        use crate::phone::Shared;

        let serving = |port, pid| crate::pool::Process {
            pid,
            name: "node".into(),
            ports: vec![port],
        };
        let tree = Tree {
            processes: vec![serving(5173, 41), serving(3000, 40)],
            ..crate::pool::stand_in(std::path::PathBuf::from("/p/1/app"))
        };
        let text = |app: &App| {
            port_spans(app, &tree)
                .iter()
                .map(|span| span.content.to_string())
                .collect::<String>()
        };
        let mut app = App::default();
        assert_eq!(text(&app), ":3000 :5173", "nothing shared");
        app.apply_shares(vec![
            Shared {
                port: 3000,
                is_public: false,
                address: "https://box.tail1.ts.net:3000".into(),
            },
            Shared {
                port: 5173,
                is_public: true,
                address: "https://box.tail1.ts.net".into(),
            },
        ]);
        assert_eq!(text(&app), ":3000p :5173P");
        let spans = port_spans(&app, &tree);
        assert_eq!(spans[1].style.fg, Some(theme().accent), "p stands out");
        assert!(
            spans[4].style.add_modifier.contains(Modifier::BOLD),
            "the public one stands out most"
        );
    }

    #[test]
    fn git_reads_as_a_prompt_writes_it_and_clean_is_blank() {
        let text = |work: Work| {
            git_spans(work)
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        };
        let mixed = Work {
            changed: 3,
            staged: 1,
            unstaged: 2,
            untracked: 1,
            unpushed: 4,
        };
        assert_eq!(text(mixed), "⇡4 +1 !2 ?1");
        let spans = git_spans(mixed);
        assert_eq!(
            spans[1].style.fg,
            Some(theme().success),
            "⇡ as p10k colours it"
        );
        assert_eq!(
            spans[7].style.fg,
            Some(theme().info),
            "? as p10k colours it"
        );
        assert_eq!(
            text(Work {
                untracked: 2,
                changed: 2,
                ..Work::default()
            }),
            "?2"
        );
        assert_eq!(text(Work::default()), "", "clean is blank");
    }

    #[test]
    fn copies_a_terminal_screen_with_its_colours_and_cursor() {
        let mut parser = vt100::Parser::new(3, 10, 0);
        parser.process(b"hi \x1b[1;31mred\x1b[0m\r\n\x1b[7mrev\x1b[0m");
        let area = Rect::new(2, 1, 10, 3);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 4));
        let cursor = draw_screen(parser.screen(), &mut buffer, area);
        let cell = |x, y| buffer.cell((x, y)).unwrap().clone();
        assert_eq!(cell(2, 1).symbol(), "h");
        assert_eq!(cell(5, 1).symbol(), "r");
        assert_eq!(cell(5, 1).fg, Color::Indexed(1));
        assert!(cell(5, 1).modifier.contains(Modifier::BOLD));
        assert!(cell(2, 2).modifier.contains(Modifier::REVERSED));
        assert_eq!(cursor, Some(Position::new(5, 2)));
        parser.process(b"\x1b[?25l");
        assert_eq!(draw_screen(parser.screen(), &mut buffer, area), None);
    }
}

#[cfg(test)]
mod sidebar_tests {
    use super::*;
    use crate::plugin::{Limit, Part};
    use crate::pool::Process;
    use crate::ship::Ship;

    const NOW: u64 = 1_793_052_000;

    fn agent() -> Agent {
        Agent {
            name: "claude".into(),
            pid: Some(1),
            status: None,
            source: crate::agents::Source::Plugin,
            is_background: true,
            attach: None,
            job: None,
            plugin: None,
            active_at: None,
            context: Some(Context {
                used: 260_000,
                window: 1_000_000,
                parts: vec![Part {
                    name: "conversation".into(),
                    tokens: 230_000,
                }],
            }),
            spend: Some(Spend {
                input: 1_204,
                cache_read: 25_300_000,
                cache_write: 412_000,
                output: 88_000,
            }),
            label: None,
        }
    }

    fn tree() -> Tree {
        Tree {
            name: "1".into(),
            branch: Some("anthonyb8/fix-four-gaps-in-treetop-and-treehouse".into()),
            work: Some(Work {
                changed: 12,
                unstaged: 12,
                unpushed: 3,
                ..Work::default()
            }),
            ship: Some(Box::new(Ship {
                branch: "b".into(),
                stage: Stage::Merged,
                label: "#12 merged".into(),
                url: Some("https://github.com/anthonyb8/treetop/pull/12".into()),
                head: None,
                detail: vec!["checks 3 passed, 1 failed".into(), "review approved".into()],
                plugin: "github".into(),
            })),
            processes: vec![
                Process {
                    pid: 41,
                    name: "vite".into(),
                    ports: vec![5173],
                },
                Process {
                    pid: 40,
                    name: "node".into(),
                    ports: vec![3000, 3000],
                },
            ],
            ..crate::pool::stand_in(std::path::PathBuf::from("/p/1/app"))
        }
    }

    fn usage() -> Usage {
        Usage {
            limits: vec![
                Limit {
                    name: "5-hour".into(),
                    percent: 82,
                    resets_at: Some(NOW + 2 * 3_600),
                },
                Limit {
                    name: "weekly Fable".into(),
                    percent: 4,
                    resets_at: None,
                },
            ],
            at: Some(NOW - 180),
        }
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn the_sidebar_shows_context_then_usage_and_spend_then_the_tree() {
        let agent = agent();
        let lines = sidebar_lines(Some(&agent), Some(&tree()), Some(&usage()), NOW, 27);
        let text = text(&lines);
        for line in &text {
            assert!(
                line.chars().count() <= 27,
                "{line:?} is wider than the sidebar"
            );
        }
        assert_eq!(
            text[7..],
            [
                "Usage  as of 3m ago",
                "5-hour       ████████░  82%",
                "             resets in 2h",
                "weekly Fable █░░░░░░░░   4%",
                "",
                "This session",
                "input                    1k",
                "cache read            25.3M",
                "cache write            412k",
                "output                  88k",
                "",
                "Tree 1",
                "⇡3 !12  #12 merged",
                "",
                "Ports",
                "localhost:3000 node",
                "localhost:5173 vite",
                "",
                "PR",
                "github.com/anthonyb8/treeto",
                "p/pull/12",
                "checks 3 passed, 1 failed",
                "review approved",
            ]
        );
    }

    #[test]
    fn a_tree_serving_nothing_with_no_pr_says_so() {
        let tree = Tree {
            ship: None,
            ..crate::pool::stand_in(std::path::PathBuf::from("/p/1/app"))
        };
        let text = text(&tree_lines(&tree, 27));
        assert_eq!(text[2..], ["", "Ports", "none", "", "PR", "none"]);
    }

    #[test]
    fn an_agent_nothing_is_known_of_says_so_and_a_tree_is_left_out_when_none() {
        let text = text(&sidebar_lines(None, None, None, NOW, 27));
        assert_eq!(
            text,
            [
                "Context",
                "not known for this agent",
                "",
                "Usage",
                "plan not known",
                "",
                "This session",
                "not known for this agent",
            ]
        );
    }

    fn run() -> crate::keel::Run {
        serde_json::from_value(serde_json::json!({
            "version": 1, "tree": "/p/1/app", "playbook": "bug-fix", "state": "active",
            "goal": "users get two notifications after a retry",
            "steps": [
                {"title": "Reproduce it", "state": "done"},
                {"title": "Narrow the cause", "state": "doing"},
                {"title": "Fix it at the root", "state": "pending"}
            ],
            "current": 2, "attention": 2, "stale": false
        }))
        .unwrap()
    }

    /// A tree with a keel session working in it under a keel run.
    fn keel_app() -> App {
        let mut app = App::new(None);
        let mut agent = agent();
        agent.name = "fix-retry".into();
        agent.label = Some("keel".into());
        agent.status = Some(Status::Busy);
        app.trees = vec![Tree {
            path: "/p/1/app".into(),
            holder: Some("keel:fix/retry".into()),
            agents: vec![agent],
            ..tree()
        }];
        app.runs = [(std::path::PathBuf::from("/p/1/app"), run())].into();
        app
    }

    #[test]
    fn the_detail_pane_shows_the_run_and_labels_a_keel_session() {
        let app = keel_app();
        let text = text(&detail_lines(&app, &app.trees[0]));
        assert!(
            text.iter().any(|l| l
                == "run       bug-fix  step 2/3 Narrow the cause  active  2 need attention  \
                    ·  users get two notifications after a retry"),
            "{text:#?}"
        );
        assert!(
            text.iter()
                .any(|l| l.starts_with("agents    ● fix-retry (keel, busy")),
            "{text:#?}"
        );
    }

    /// Everything on screen, row by row.
    fn screen(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, app, None, None, None))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn a_run_column_shows_only_once_keel_lists_a_run() {
        let mut app = keel_app();
        let shown = screen(&app, 160, 24);
        let header = shown.iter().find(|l| l.contains("BRANCH")).unwrap();
        assert!(header.contains("RUN"), "{header}");
        let row = shown
            .iter()
            .find(|l| l.contains("fix-retry [keel]"))
            .unwrap();
        assert!(row.contains("bug-fix 2/3 !2"), "{row}");
        app.runs.clear();
        let shown = screen(&app, 160, 24);
        let header = shown.iter().find(|l| l.contains("BRANCH")).unwrap();
        assert!(!header.contains("RUN"), "{header}");
    }

    #[test]
    fn the_return_dialog_warns_of_a_run_that_holds_the_tree() {
        let mut app = keel_app();
        app.pending = Some(Pending {
            trees: app.trees.clone(),
            runs: app.runs.clone(),
        });
        let shown = screen(&app, 120, 30).join("\n");
        assert!(
            shown.contains("keel run bug-fix 2/3 · 2 need attention holds it"),
            "{shown}"
        );
        assert!(shown.contains("listed until `keel run stop`"), "{shown}");
        app.pending.as_mut().unwrap().runs.clear();
        let shown = screen(&app, 120, 30).join("\n");
        assert!(!shown.contains("keel run"), "{shown}");
    }
}

#[cfg(test)]
mod inbox_tests {
    use super::*;
    use crate::inbox::tests::app;
    use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    fn screen(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, app, None, None, None))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    /// Clicks where `text` is drawn on screen.
    fn click_on(app: &mut App, width: u16, height: u16, text: &str) -> Option<Outcome> {
        let shown = screen(app, width, height);
        let (row, line) = shown
            .iter()
            .enumerate()
            .find(|(_, l)| l.contains(text))
            .unwrap_or_else(|| panic!("{text} is not on screen:\n{}", shown.join("\n")));
        let column = line[..line.find(text).unwrap()].chars().count();
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: u16::try_from(column).unwrap(),
            row: u16::try_from(row).unwrap(),
            modifiers: KeyModifiers::NONE,
        };
        click(app, Rect::new(0, 0, width, height), mouse)
    }

    #[test]
    fn a_click_opens_the_entry_drawn_under_it() {
        let mut app = app();
        assert_eq!(
            click_on(&mut app, 200, 50, "endodeals-web 12"),
            Some(Outcome::SwitchTo {
                checkout: "/code/endodeals-web".into(),
                tree: "/other/12/web".into(),
                kind: Kind::NeedsYou,
            })
        );
        let mut app = self::app();
        assert!(matches!(
            click_on(&mut app, 200, 50, "Where should the inbox sit?"),
            Some(Outcome::Chat { tree, .. }) if tree == "4"
        ));
    }

    #[test]
    fn a_narrow_terminal_shows_counts_and_a_click_on_them_opens_the_inbox_over_the_list() {
        let mut app = app();
        let shown = screen(&app, 120, 30).join("\n");
        assert!(!shown.contains("INBOX"), "{shown}");
        for count in ["●2", "✗2", "✓1"] {
            assert!(shown.contains(count), "{count} in\n{shown}");
        }
        assert_eq!(click_on(&mut app, 120, 30, "●2"), Some(Outcome::Continue));
        assert_eq!(app.rail, Some(0));
        let shown = screen(&app, 120, 30).join("\n");
        assert!(shown.contains("INBOX"), "{shown}");
        assert!(shown.contains("Enter open  x dismiss  Esc back"), "{shown}");
    }

    #[test]
    fn the_cursor_covers_both_lines_of_its_entry_to_the_edge() {
        let mut app = app();
        app.rail = Some(0);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, 50)).unwrap();
        terminal
            .draw(|frame| draw(frame, &app, None, None, None))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let lines = screen(&app, 200, 50);
        let first = lines.iter().position(|l| l.contains("4 ANB-41")).unwrap();
        let cursor = theme().cursor.bg;
        // The rail's last column, inside its border and padding, on each of
        // the entry's two lines.
        for row in [first, first + 1] {
            let cell = &buffer[(197, u16::try_from(row).unwrap())];
            assert_eq!(
                cell.bg,
                cursor.unwrap_or_default(),
                "row {row}: {}",
                lines[row]
            );
        }
    }

    #[test]
    fn the_picker_counts_what_waits_unseen_in_each_workspace() {
        let mut app = app();
        app.checkout = "/code/treetop".into();
        let choice = |name: &str, checkout: &str| crate::app::Choice {
            checkout: checkout.into(),
            name: name.into(),
            held: Some(2),
        };
        app.picker = Some(Picker {
            choices: vec![
                choice("treetop", "/code/treetop"),
                choice("endodeals-web", "/code/endodeals-web"),
                choice("lander", "/code/lander"),
            ],
            selected: 0,
        });
        // The picker's rows are the lines that end in their checkout.
        let row = |shown: &[String], name: &str| {
            shown
                .iter()
                .find(|l| l.contains(&format!("/code/{name}")))
                .unwrap()
                .clone()
        };
        let shown = screen(&app, 200, 30);
        assert!(
            row(&shown, "treetop").contains("●1 ✗2 ✓1"),
            "{}",
            row(&shown, "treetop")
        );
        assert!(
            row(&shown, "endodeals-web").contains("●1"),
            "{}",
            row(&shown, "endodeals-web")
        );
        assert!(
            !row(&shown, "lander").contains('●'),
            "{}",
            row(&shown, "lander")
        );
        // Tree 4's ask, once its chat has opened, no longer counts.
        app.saw_chat(&["claude".into(), "attach".into(), "j4".into()]);
        let shown = screen(&app, 200, 30);
        let treetop = row(&shown, "treetop");
        assert!(
            treetop.contains("✗2 ✓1") && !treetop.contains("●1"),
            "{treetop}"
        );
    }

    #[test]
    fn a_popup_dims_the_list_but_leaves_the_inbox_bright_beside_it() {
        let mut app = app();
        app.picker = Some(Picker {
            choices: Vec::new(),
            selected: 0,
        });
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, 30)).unwrap();
        terminal
            .draw(|frame| draw(frame, &app, None, None, None))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let lines = screen(&app, 200, 30);
        let row = lines.iter().position(|l| l.contains("4 ANB-41")).unwrap();
        let column = lines[row][..lines[row].find("4 ANB-41").unwrap()]
            .chars()
            .count();
        let at = |x: usize| &buffer[(u16::try_from(x).unwrap(), u16::try_from(row).unwrap())];
        assert!(
            !at(column).modifier.contains(Modifier::DIM),
            "the inbox stays bright"
        );
        assert!(
            at(4).modifier.contains(Modifier::DIM),
            "the list under the popup dims"
        );
    }

    #[test]
    fn a_chat_opens_left_of_the_inbox_and_takes_the_width_back_once_it_hides() {
        let mut app = app();
        let screen = Rect::new(0, 0, 200, 30);
        let inbox_starts = 200 - INBOX_WIDTH;
        // The chat's popup, its border and sidebar included.
        assert!(popup::area(stage(&app, screen)).right() <= inbox_starts);
        app.inbox.is_hidden = true;
        assert!(popup::area(stage(&app, screen)).right() > inbox_starts);
    }

    #[test]
    fn hiding_the_inbox_gives_the_list_its_width_back() {
        let mut app = app();
        // Where the table's window ends, at its top right corner.
        let wide = |app: &App| {
            let top = &screen(app, 200, 30)[0];
            top[..top.find('┐').unwrap()].chars().count()
        };
        let beside = wide(&app);
        app.handle_key(ratatui::crossterm::event::KeyEvent::from(
            ratatui::crossterm::event::KeyCode::Char('E'),
        ));
        let shown = screen(&app, 200, 30).join("\n");
        assert!(!shown.contains("INBOX"), "{shown}");
        assert!(!shown.contains("●2"), "no strip either:\n{shown}");
        assert!(wide(&app) > beside, "the table's window takes the width");
    }

    /// The screen as drawn, cell by cell, for what a cell's colours are.
    fn cells(app: &App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, app, None, None, None))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn the_inbox_carries_the_list_s_heading_bar_on_with_its_icon_folded_or_not() {
        let app = app();
        let header = theme().header.bg;
        for width in [120, 200] {
            let shown = screen(&app, width, 20);
            let row = shown
                .iter()
                .position(|l| l.contains("BRANCH"))
                .expect("the list's headings");
            assert!(shown[row].contains('✉'), "{}", shown[row]);
            let drawn = cells(&app, width, 20);
            let row = u16::try_from(row).unwrap();
            let icon = (0..width)
                .find(|&x| drawn[(x, row)].symbol() == "✉")
                .unwrap();
            assert_eq!(
                drawn[(icon, row)].fg,
                theme().warning,
                "something needs you"
            );
            // Across the inbox to its right border.
            for x in [icon - 1, width - 2] {
                assert_eq!(drawn[(x, row)].bg, header.unwrap(), "at {x} of {width}");
            }
            let divider = (0..icon)
                .rev()
                .find(|&x| drawn[(x, row)].symbol() == "│")
                .unwrap();
            assert_eq!(
                drawn[(divider, row)].fg,
                theme().border,
                "the divider stays dim"
            );
        }
        assert!(screen(&app, 200, 20).iter().any(|l| l.contains("✉ INBOX")));
    }
}

/// The PR popup as a user goes through it: `v`, each tab in turn, and what
/// it says when there is no PR or it cannot read one (ANB-51).
#[cfg(test)]
mod pr_view_tests {
    use super::*;
    use crate::app::Outcome;
    use crate::coderabbit::{Finding, Kind as Found};
    use crate::inbox::tests::app;
    use crate::plugin;
    use crate::ship::{BotReview, CheckView, Outcome as Checked, PrView, Thread};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn screen(app: &App, width: u16, height: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, app, None, None, None))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn press(app: &mut App, code: KeyCode) -> Outcome {
        app.handle_key(KeyEvent::from(code))
    }

    fn pr() -> PrView {
        PrView {
            number: 37,
            title: "Highlight syntax in the diff".into(),
            state: "OPEN".into(),
            author: "me".into(),
            base: "main".into(),
            branch: "me/anb-41-inbox-rail".into(),
            head: "abc1234".into(),
            body: "Colours code in the diff.".into(),
            commits: 3,
            review: "required".into(),
            merge: "blocked".into(),
            checks: vec![
                CheckView {
                    name: "clippy".into(),
                    outcome: Checked::Failed,
                    workflow: "CI".into(),
                    seconds: Some(72),
                    log: Some("error: match can be simplified".into()),
                    ..CheckView::default()
                },
                CheckView {
                    name: "test".into(),
                    outcome: Checked::Passed,
                    seconds: Some(220),
                    ..CheckView::default()
                },
            ],
            threads: vec![Thread {
                author: "jlin".into(),
                path: Some("src/theme.rs".into()),
                line: Some(111),
                code: vec!["+        syntax: Some(SyntaxPalette {".into()],
                code_line: Some(111),
                body: "Should the ANSI theme get one too?".into(),
                ..Thread::default()
            }],
            bot: Some(BotReview {
                reviewed: Some("abc1234".into()),
                findings: vec![
                    Finding {
                        kind: Found::Issue,
                        severity: Some("Major".into()),
                        path: Some("src/diff.rs".into()),
                        lines: Some("236-251".into()),
                        title: "Highlighting can stall a long diff".into(),
                        body: "Cap it.".into(),
                        fix: vec!["-let a = 1;".into(), "+let a = 2;".into()],
                        ..Finding::default()
                    },
                    Finding {
                        kind: Found::Nitpick,
                        title: "Name the closure".into(),
                        ..Finding::default()
                    },
                ],
                ..BotReview::default()
            }),
            ..PrView::default()
        }
    }

    #[test]
    fn the_pr_view_shows_each_tab_and_says_why_when_it_cannot() {
        let mut app = app();
        let path = std::path::PathBuf::from("/pool/4/app");
        while app.current().is_none_or(|t| t.path != path) {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('v'));
        let shown = screen(&app, 200, 40);
        assert!(shown.contains("reading the PR..."), "{shown}");

        app.pr_loaded(path.clone(), Ok(Some(pr())));
        let shown = screen(&app, 200, 40);
        for seen in [
            "#37 Highlight syntax in the diff",
            "1 Overview",
            "2 Checks ✗1",
            "4 CodeRabbit 1",
            "✗ 1 failed: clippy",
            "1 issue, 0 suggestions, 1 nitpick",
            "Colours code in the diff.",
        ] {
            assert!(shown.contains(seen), "{seen} on the Overview:\n{shown}");
        }

        press(&mut app, KeyCode::Char('2'));
        let shown = screen(&app, 200, 40);
        for seen in [
            "clippy",
            "1m 12s",
            "clippy · the end of its log",
            "error: match can be simplified",
        ] {
            assert!(shown.contains(seen), "{seen} on Checks:\n{shown}");
        }

        press(&mut app, KeyCode::Char('3'));
        let shown = screen(&app, 200, 40);
        for seen in [
            "jlin on src/theme.rs:111",
            "111 +",
            "Should the ANSI theme get one too?",
        ] {
            assert!(shown.contains(seen), "{seen} on Comments:\n{shown}");
        }

        press(&mut app, KeyCode::Char('4'));
        let shown = screen(&app, 200, 40);
        for seen in [
            "coderabbitai reviewed abc1234, the head",
            "src/diff.rs:236-251",
            "Highlighting can stall a long diff",
            "Suggested change",
            "1 nitpick   z shows them",
        ] {
            assert!(shown.contains(seen), "{seen} on CodeRabbit:\n{shown}");
        }
        assert!(
            !shown.contains("Name the closure"),
            "nitpicks fold:\n{shown}"
        );
        let narrow = screen(&app, 100, 40);
        assert!(
            narrow.contains("4 CodeRabbit"),
            "the tabs fit at 100 columns:\n{narrow}"
        );

        app.pr_loaded(path.clone(), Ok(None));
        let shown = screen(&app, 200, 40);
        assert!(
            shown.contains("No pull request for me/anb-41-inbox-rail yet.")
                && shown.contains("Esc, then s, starts a ship run"),
            "{shown}"
        );
        app.pr_loaded(
            path,
            Err(plugin::Error::new(plugin::Code::Auth, "run gh auth login")),
        );
        let shown = screen(&app, 200, 40);
        assert!(shown.contains("gh is not signed in"), "{shown}");
        // Esc gives the keys back to the table and leaves the PR in its tab.
        press(&mut app, KeyCode::Esc);
        assert!(!app.is_window_focused);
        assert!(screen(&app, 200, 40).contains("gh is not signed in"));
    }

    #[test]
    fn a_real_prs_text_fits_and_reads_at_100_columns() {
        let mut app = app();
        let path = std::path::PathBuf::from("/pool/4/app");
        while app.current().is_none_or(|t| t.path != path) {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('v'));
        let mut pr = pr();
        pr.state = "MERGED".into();
        pr.title = "refactor(core): order restriction levels and rename the shown-handles op \
                    across every crate in the workspace"
            .into();
        pr.body =
            "Bumps the workspace to 0.8.0 and updates the examples, the Rust dependency snippets \
                   and the docs that name the version, so nothing still points at the old one."
                .into();
        pr.checks[0].log = Some(
            "ruby scripts/check-workflow-triggers.rb --strict --all-workflows-in-the-repository \
             ##[error]only workflow_dispatch is permitted"
                .into(),
        );
        pr.threads.push(Thread {
            author: "jlin".into(),
            path: Some("Cargo.toml".into()),
            line: Some(16),
            resolved: true,
            body: "Does default-fancy pull in onig?".into(),
            ..Thread::default()
        });
        if let Some(bot) = pr.bot.as_mut() {
            bot.findings[0].title = "Require 0.2.3 in the standalone example docs".into();
            bot.findings[0].path = Some("docs/crs.md".into());
            bot.findings[0].lines = Some("70".into());
        }
        app.pr_loaded(path.clone(), Ok(Some(pr)));

        let shown = screen(&app, 100, 40);
        let rows: Vec<&str> = shown.lines().collect();
        let header = rows
            .iter()
            .find(|r| r.contains(" pr "))
            .expect("the PR's bar");
        assert!(
            header.contains('…'),
            "a long title is cut with an ellipsis: {header}"
        );
        let left = rows
            .iter()
            .find_map(|r| r.find('┌').map(|b| r[..b].chars().count()))
            .expect("the popup's corner");
        for row in rows
            .iter()
            .filter(|r| r.contains("the docs") || r.contains("examples"))
        {
            let after: Vec<char> = row.chars().skip(left + 1).take(1).collect();
            assert_eq!(after, [' '], "wrapped lines keep their indent: {row}");
        }
        assert!(shown.contains("me merged 3 commits into main"), "{shown}");

        press(&mut app, KeyCode::Char('2'));
        let shown = screen(&app, 100, 40);
        assert!(
            shown.contains("only workflow_dispatch is permitted"),
            "a long log line wraps:\n{shown}"
        );

        press(&mut app, KeyCode::Char('3'));
        let shown = screen(&app, 100, 40);
        let folded: Vec<&str> = shown
            .lines()
            .filter(|r| r.contains("Cargo.toml:16"))
            .collect();
        assert!(
            folded.len() == 1 && folded[0].contains("resolved"),
            "a resolved thread folds to one row: {folded:?}"
        );

        press(&mut app, KeyCode::Char('4'));
        let shown = screen(&app, 100, 40);
        assert!(
            shown.contains("Require 0.2.3 in the standalone example docs"),
            "the selected finding's title shows whole:\n{shown}"
        );
        assert!(
            shown.contains("1 nitpick ") && !shown.contains("1 nitpicks"),
            "{shown}"
        );

        app.pr_loaded(
            path,
            Err(plugin::Error::new(plugin::Code::Auth, "run gh auth login")),
        );
        let shown = screen(&app, 100, 40);
        assert!(
            shown.contains("Run gh auth login.") && !shown.contains("then ^R"),
            "^R is named once:\n{shown}"
        );
    }

    #[test]
    fn long_and_marked_up_items_read_whole_at_100_columns() {
        let mut app = app();
        let path = std::path::PathBuf::from("/pool/4/app");
        while app.current().is_none_or(|t| t.path != path) {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('v'));
        let mut pr = pr();
        pr.title =
            "fix(registry-restore): continue restoring remaining keys when one snapshot fails"
                .into();
        pr.checks[1].outcome = Checked::Failed;
        pr.checks[1].name =
            "CodeScene Code Health Review (issue-1066-a-very-long-branch-with-more-to-it)".into();
        pr.threads[0].body = "\u{274c} New issue: [**Complex Method**](https://codescene.io/x) in <sub>x.rs</sub> <!-- hidden -->".into();
        if let Some(bot) = pr.bot.as_mut() {
            bot.reviewing = true;
            bot.reviewed = Some("old5678".into());
            let mut fix: Vec<String> = (0..60).map(|i| format!("+    step {i};")).collect();
            fix.push("+    launch npm start -- --fullscreen --keep-the-window-open --and-a-flag END_OF_LINE".into());
            fix.push("+the very last line".into());
            bot.findings[0].fix = fix;
            bot.findings.insert(
                1,
                Finding {
                    kind: Found::Suggestion,
                    severity: Some("Minor".into()),
                    path: Some("src/b.rs".into()),
                    title: "Already addressed".into(),
                    resolved: true,
                    ..Finding::default()
                },
            );
        }
        app.pr_loaded(path.clone(), Ok(Some(pr)));

        let shown = screen(&app, 100, 40);
        let rows: Vec<&str> = shown.lines().collect();
        // The window's own corner, round its tab bar and the PR.
        let body = window_chat_area(&app, Rect::new(0, 0, 100, 40)).unwrap();
        let corner = usize::from(body.y - 2);
        let left = usize::from(body.x - 1);
        assert_eq!(
            rows[corner].chars().nth(left),
            Some('┌'),
            "the window is drawn there"
        );
        let right = left
            + rows[corner]
                .chars()
                .skip(left)
                .take_while(|c| *c != '┐')
                .count();
        let checks = rows
            .iter()
            .position(|r| r.contains("Checks      "))
            .expect("the Checks row");
        let value = |row: &str| row.chars().skip(left + 1).position(|c| c != ' ');
        let first: String = rows[checks].chars().skip(left + 1).collect();
        let column = first.find('✗').map(|b| first[..b].chars().count());
        assert_eq!(
            value(rows[checks + 1]),
            column,
            "a wrapped value starts under the value column: {}",
            rows[checks + 1]
        );
        for row in &rows[corner + 1..corner + 30] {
            let before: Vec<char> = row.chars().skip(right - 1).take(1).collect();
            assert!(
                before == [' '] || before == ['─'] || before == ['│'],
                "a margin at the right: {row}"
            );
        }

        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        let shown = screen(&app, 100, 40);
        let bar = shown.lines().find(|r| r.contains(" pr ")).unwrap();
        assert!(
            bar.contains("reading…"),
            "a reload shows beside a long title: {bar}"
        );
        app.pr_loaded(
            path.clone(),
            Ok(app.prs[&path].pr.clone().unwrap().unwrap()),
        );

        press(&mut app, KeyCode::Char('3'));
        let shown = screen(&app, 100, 40);
        assert!(
            shown.contains("New issue: Complex Method") && shown.contains("in x.rs"),
            "{shown}"
        );
        for raw in ["**", "<sub>", "<!--", "](https"] {
            assert!(!shown.contains(raw), "{raw} is markup, not text:\n{shown}");
        }

        press(&mut app, KeyCode::Char('4'));
        let shown = screen(&app, 100, 40);
        assert!(
            shown.contains("reviewing the newest push"),
            "a tall first item hides nothing above it:\n{shown}"
        );
        assert!(shown.contains("Esc"), "the key bar keeps Esc:\n{shown}");
        let tag = |shown: &str, word: &str| {
            let row = shown.lines().find(|r| r.contains(word))?;
            row.find(word).map(|b| row[..b].chars().count())
        };
        let open = tag(&shown, "issue ");
        press(&mut app, KeyCode::Char('j'));
        let resolved = tag(&screen(&app, 100, 40), "suggestion ");
        press(&mut app, KeyCode::Char('k'));
        assert!(
            open.is_some() && open == resolved,
            "resolved rows line up with open ones"
        );
        for _ in 0..12 {
            press(&mut app, KeyCode::Char('d'));
        }
        let shown = screen(&app, 100, 40);
        assert!(
            shown.contains("the very last line"),
            "d scrolls to a long item's end:\n{shown}"
        );
        assert!(
            shown.contains("END_OF_LINE"),
            "a long code line wraps:\n{shown}"
        );
    }

    #[test]
    fn running_checks_and_long_places_read_right() {
        let mut app = app();
        let path = std::path::PathBuf::from("/pool/4/app");
        while app.current().is_none_or(|t| t.path != path) {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('v'));
        let mut pr = pr();
        pr.merge = "unstable".into();
        pr.checks[0].outcome = Checked::Pending;
        pr.checks[0].log = None;
        if let Some(bot) = pr.bot.as_mut() {
            bot.findings[0].path =
                Some("apps/android/app/src/main/java/app/switchboard/mobile/ui/navigation/SwitchboardNavigation.kt".into());
            bot.findings[0].lines = Some("1613-1620".into());
        }
        app.pr_loaded(path, Ok(Some(pr)));
        let shown = screen(&app, 100, 40);
        assert!(
            !shown.contains("checks failing") && shown.contains("waiting on checks"),
            "a running check is not a failing one:\n{shown}"
        );
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('4'));
        press(&mut app, KeyCode::Char('j'));
        let shown = screen(&app, 100, 40);
        let row = shown
            .lines()
            .find(|r| r.contains("SwitchboardNavigation.kt"))
            .expect("the finding's row");
        assert!(
            row.contains("…/") && row.contains(":1613-1620") && row.contains('…'),
            "a long place keeps its file name and lines, and the title its ellipsis: {row}"
        );
    }

    #[test]
    fn emoji_bullets_and_the_no_pr_note_keep_their_columns() {
        let mut app = app();
        let path = std::path::PathBuf::from("/pool/4/app");
        while app.current().is_none_or(|t| t.path != path) {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('v'));
        let mut pr = pr();
        pr.checks[0].name = "\u{2705} Merge Gate".into();
        pr.checks[1].name = "Analyze (actions)".into();
        pr.body = "- einteger with a NaN or inf scalar: the scalar converts to 0, the edecimal \
                   convention, and this was already true for every other operator we have."
            .into();
        app.pr_loaded(path.clone(), Ok(Some(pr)));
        let shown = screen(&app, 100, 40);
        let rows: Vec<&str> = shown.lines().collect();
        let bullet = rows
            .iter()
            .position(|r| r.contains("einteger"))
            .expect("the bullet");
        // The screen has a cell a char, a wide one's filler cell included.
        let text_column = |row: &str, word: &str| row.find(word).map(|b| row[..b].chars().count());
        let start = text_column(rows[bullet], "einteger");
        let next = rows[bullet + 1];
        let continued = next.chars().nth(start.unwrap_or(0));
        let before: String = next.chars().take(start.unwrap_or(0)).collect();
        assert!(
            continued.is_some_and(|c| c != ' ') && before.ends_with(' '),
            "a wrapped bullet continues under its text: {}\n{next}",
            rows[bullet]
        );

        press(&mut app, KeyCode::Char('2'));
        let shown = screen(&app, 100, 40);
        let time_column = |word: &str| {
            let row = shown.lines().find(|r| r.contains(word))?;
            text_column(row, word)
        };
        assert_eq!(
            time_column("1m 12s"),
            time_column("3m 40s"),
            "an emoji in a name keeps the columns in line:\n{shown}"
        );

        app.pr_loaded(path, Ok(None));
        let shown = screen(&app, 100, 40);
        let body = window_chat_area(&app, Rect::new(0, 0, 100, 40)).unwrap();
        let left = usize::from(body.x - 1);
        assert_eq!(
            shown
                .lines()
                .nth(usize::from(body.y - 2))
                .and_then(|r| r.chars().nth(left)),
            Some('┌'),
            "the window is drawn there"
        );
        for row in shown
            .lines()
            .filter(|r| r.contains("yet.") || r.contains("one."))
        {
            let after: Vec<char> = row.chars().skip(left + 1).take(1).collect();
            assert_eq!(
                after,
                [' '],
                "the note keeps its margin when it wraps: {row}"
            );
        }
    }

    #[test]
    fn reloads_moved_threads_tables_and_counts_read_true() {
        let mut app = app();
        let path = std::path::PathBuf::from("/pool/4/app");
        while app.current().is_none_or(|t| t.path != path) {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('v'));
        app.pr_loaded(path.clone(), Ok(None));
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        let shown = screen(&app, 100, 40);
        assert!(
            shown.contains("reading"),
            "^R on the no-PR note says it is reading:\n{shown}"
        );

        let mut pr = pr();
        pr.threads[0].line = Some(3594);
        pr.threads[0].code_line = Some(3509);
        pr.threads.push(Thread {
            author: "sourcery-ai".into(),
            body: "| Change | Details |\n| ------ | ------- |\n| Route jobs | Replace the runner |"
                .into(),
            ..Thread::default()
        });
        app.pr_loaded(path, Ok(Some(pr)));
        let shown = screen(&app, 100, 40);
        assert!(
            shown.contains("3 Comments 2") && shown.contains("1 open, 0 resolved, 1 on the PR"),
            "the tab and the Overview count the same comments:\n{shown}"
        );
        press(&mut app, KeyCode::Char('3'));
        let shown = screen(&app, 100, 40);
        assert!(
            shown.contains("on src/theme.rs:3509 (now 3594)"),
            "the header names the line its code is numbered from:\n{shown}"
        );
        assert!(
            !shown
                .lines()
                .any(|r| r.contains("Details") && r.contains("Route jobs")),
            "a table keeps its rows:\n{shown}"
        );
        assert!(
            shown.contains("| Route jobs | Replace the runner |"),
            "{shown}"
        );
    }

    #[test]
    fn tables_quotes_rereviews_and_tabs_draw_cleanly() {
        let mut app = app();
        let path = std::path::PathBuf::from("/pool/4/app");
        while app.current().is_none_or(|t| t.path != path) {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('v'));
        let mut pr = pr();
        pr.body =
            "| Condition | On master |\n| --- | --- |\n| radius timestamp | never true |".into();
        pr.threads[0].body = "> - CI issue: smoke e2e tests (production) are red because three of them failed: \
            extension-installation-smoke, onboarding-smoke and settings-smoke\n\
            > - packages/renderer/src/lib/preferences/a-path-longer-than-any-row-in-the-popup-could-hold/rendering-task.spec.ts:92 issue"
            .into();
        pr.threads[0].code = vec!["+\tif (attr->name.txt) {".into()];
        if let Some(bot) = pr.bot.as_mut() {
            bot.reviewed = Some(pr.head.clone());
            bot.reviewing = true;
        }
        app.pr_loaded(path, Ok(Some(pr)));
        let shown = screen(&app, 100, 40);
        assert!(
            !shown
                .lines()
                .any(|r| r.contains("Condition") && r.contains("radius")),
            "a table in the description keeps its rows:\n{shown}"
        );

        press(&mut app, KeyCode::Char('3'));
        let shown = screen(&app, 100, 40);
        let bars = |row: &str| row.matches('│').count();
        let first = shown
            .lines()
            .find(|r| r.contains("CI issue"))
            .expect("the quote");
        for row in shown
            .lines()
            .filter(|r| r.contains("onboarding") || r.contains("settings-smoke"))
        {
            assert_eq!(
                bars(row),
                bars(first),
                "a wrapped quote keeps its bar: {row}"
            );
        }
        assert!(
            !shown.lines().any(|r| r.replace(['│', ' '], "") == "•"),
            "a bullet stays with its text:\n{shown}"
        );
        assert!(!shown.contains('\t'), "tabs are spaces:\n{shown}");

        press(&mut app, KeyCode::Char('4'));
        let shown = screen(&app, 100, 40);
        assert!(
            !shown.contains("newest push") && shown.contains("reviewing it again"),
            "a re-review of the head is not a newer push:\n{shown}"
        );
    }

    #[test]
    fn names_rereviews_and_links_use_the_room_they_have() {
        let mut app = app();
        let path = std::path::PathBuf::from("/pool/4/app");
        while app.current().is_none_or(|t| t.path != path) {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('v'));
        let mut pr = pr();
        pr.checks[0].name = "Test on java-version 8.0.x and os ubuntu-latest".into();
        pr.checks[1].name = "Test on java-version 8.0.x and os windows-latest".into();
        pr.checks[0].log = Some(
            "Process 'command git' finished with non-zero exit value 255 after a long while".into(),
        );
        pr.threads[0].body = "Extend the guard to cover the whole verdict payload. ([raw.githubusercontent.com](https://raw.githubusercontent.com/o/r/a8/src/verdict.ts))".into();
        if let Some(bot) = pr.bot.as_mut() {
            bot.reviewed = Some(pr.head.clone());
            bot.reviewing = true;
        }
        app.pr_loaded(path, Ok(Some(pr)));
        let shown = screen(&app, 100, 40);
        assert!(
            !shown.contains("newest push") && shown.contains("reviewing it again"),
            "the Overview says what the tab says:\n{shown}"
        );
        press(&mut app, KeyCode::Char('2'));
        let shown = screen(&app, 100, 40);
        assert!(
            shown.contains("os ubuntu-latest") && shown.contains("os windows-latest"),
            "names use the room the row has:\n{shown}"
        );
        assert!(
            !shown.contains("wit\n")
                && !shown
                    .lines()
                    .any(|r| r.trim_start_matches(['│', ' ']).starts_with("h non-zero")),
            "logs wrap at words:\n{shown}"
        );
        press(&mut app, KeyCode::Char('3'));
        let shown = screen(&app, 100, 40);
        assert!(
            !shown
                .lines()
                .any(|r| r.trim_end_matches(['│', ' ']).ends_with('(')),
            "a wrap falls at a space, not between styles:\n{shown}"
        );
    }

    #[test]
    fn a_long_thread_place_keeps_its_file_and_line() {
        let mut app = app();
        let path = std::path::PathBuf::from("/pool/4/app");
        while app.current().is_none_or(|t| t.path != path) {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('v'));
        let mut pr = pr();
        pr.threads[0].author = "copilot-pull-request-reviewer".into();
        pr.threads[0].path = Some("src/features/repository/insights/primitives.tsx".into());
        pr.threads[0].line = Some(194);
        pr.threads[0].code_line = Some(194);
        pr.threads[0].resolved = true;
        pr.threads.push(Thread {
            author: "jlin".into(),
            body: "Thanks.".into(),
            ..Thread::default()
        });
        app.pr_loaded(path, Ok(Some(pr)));
        press(&mut app, KeyCode::Char('3'));
        press(&mut app, KeyCode::Char('j'));
        let shown = screen(&app, 100, 40);
        let row = shown
            .lines()
            .find(|r| r.contains("copilot"))
            .expect("the thread");
        assert!(
            row.contains("primitives.tsx:194") && row.contains("resolved"),
            "a long place keeps its file and line, and the fold its note: {row}"
        );
    }

    #[test]
    fn logs_keep_indents_and_tables_read_as_text() {
        let mut app = app();
        let path = std::path::PathBuf::from("/pool/4/app");
        while app.current().is_none_or(|t| t.path != path) {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('v'));
        let mut pr = pr();
        pr.checks[0].log =
            Some("{\n   \"schemaVersion\": 2,\n     \"package\": \"bulma\"\n}".into());
        pr.threads[0].body =
            "| [Files with missing lines](https://app.codecov.io/gh/o/r/pull/1) | Patch % |\n\
            |---|---|\n| [operator.go](https://app.codecov.io/x) :x: | 50% |"
                .into();
        if let Some(bot) = pr.bot.as_mut() {
            bot.findings[0].title = "Update the description for `WorkflowStatus.Suspended`".into();
        }
        app.pr_loaded(path, Ok(Some(pr)));
        press(&mut app, KeyCode::Char('2'));
        let shown = screen(&app, 100, 40);
        assert!(
            shown.contains("   \"schemaVersion\": 2,"),
            "a log keeps its indent:\n{shown}"
        );
        press(&mut app, KeyCode::Char('3'));
        let shown = screen(&app, 100, 40);
        assert!(
            shown.contains("| Files with missing lines | Patch % |")
                && shown.contains("operator.go ❌"),
            "a table reads as text, links as their words and :x: as its mark:\n{shown}"
        );
        assert!(!shown.contains("](https"), "{shown}");
        press(&mut app, KeyCode::Char('4'));
        let shown = screen(&app, 100, 40);
        assert!(
            shown.contains("WorkflowStatus.Suspended") && !shown.contains("`WorkflowStatus"),
            "a title's code shows without its backticks:\n{shown}"
        );
    }

    #[test]
    fn wide_characters_wrap_by_the_columns_they_take() {
        let mut app = app();
        let path = std::path::PathBuf::from("/pool/4/app");
        while app.current().is_none_or(|t| t.path != path) {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('v'));
        let mut pr = pr();
        if let Some(bot) = pr.bot.as_mut() {
            bot.findings[0].body =
                "上传结果需要权限，整个作业随之变红，即使声明了 `security-events: write` 也一样，\
                而且整个作业随之变红，即使没有发现任何泄露也会失败。"
                    .into();
        }
        app.pr_loaded(path, Ok(Some(pr)));
        press(&mut app, KeyCode::Char('4'));
        let shown = screen(&app, 100, 40);
        // The screen has a cell a char, so a wide char's filler is a space.
        let text: String = shown.split_whitespace().collect();
        for words in [
            "security-events:write",
            "也一样",
            "没有发现任何泄露也会失败。",
        ] {
            let squeezed: String = words.split_whitespace().collect();
            assert!(text.contains(&squeezed), "{words} is drawn whole:\n{shown}");
        }
    }

    #[test]
    fn a_first_review_under_way_says_so_once() {
        let mut app = app();
        let path = std::path::PathBuf::from("/pool/4/app");
        while app.current().is_none_or(|t| t.path != path) {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('v'));
        let mut pr = pr();
        pr.bot = Some(BotReview {
            reviewing: true,
            ..BotReview::default()
        });
        app.pr_loaded(path, Ok(Some(pr)));
        press(&mut app, KeyCode::Char('4'));
        let shown = screen(&app, 100, 40);
        assert!(
            shown.contains("CodeRabbit is reviewing this PR for the first time."),
            "{shown}"
        );
        for said in ["has not reviewed", "from before it", "no comments to show"] {
            assert!(!shown.contains(said), "{said} contradicts it:\n{shown}");
        }
    }

    #[test]
    fn a_long_check_name_leaves_its_detail_whole() {
        let mut app = app();
        let path = std::path::PathBuf::from("/pool/4/app");
        while app.current().is_none_or(|t| t.path != path) {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('v'));
        let mut pr = pr();
        pr.checks[0].name =
            "Code standards on changed lines (ruff, function length, nesting) (non-required)"
                .into();
        pr.checks[1].name =
            "W3 reference goldens re-recorded with the replaced libraries (PRD-262) (non-required)"
                .into();
        pr.checks[1].outcome = Checked::Skipped;
        app.pr_loaded(path, Ok(Some(pr)));
        press(&mut app, KeyCode::Char('2'));
        let shown = screen(&app, 100, 60);
        // The window's rows run on as one text, wherever a heading wraps.
        let text = shown
            .lines()
            .map(|row| row.trim_matches(|c| c == '│' || c == ' '))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            text.contains("(non-required) · the end of its log ─"),
            "the heading keeps its end:\n{shown}"
        );
        press(&mut app, KeyCode::Char('j'));
        let shown = screen(&app, 100, 60);
        let text = shown
            .lines()
            .map(|row| row.trim_matches(|c| c == '│' || c == ' '))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            text.contains("(PRD-262) (non-required) was skipped in 3m 40s."),
            "the detail line wraps whole:\n{shown}"
        );
    }

    #[test]
    fn one_file_is_one_file() {
        let mut app = app();
        let path = std::path::PathBuf::from("/pool/4/app");
        while app.current().is_none_or(|t| t.path != path) {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('v'));
        let mut pr = pr();
        pr.files = 1;
        app.pr_loaded(path, Ok(Some(pr)));
        let shown = screen(&app, 200, 40);
        assert!(
            shown.contains("in 1 file") && !shown.contains("1 files"),
            "{shown}"
        );
    }

    #[test]
    fn cancelled_checks_and_unread_logs_say_what_they_are() {
        let mut app = app();
        let path = std::path::PathBuf::from("/pool/4/app");
        while app.current().is_none_or(|t| t.path != path) {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('v'));
        let mut pr = pr();
        pr.checks[0].log = None;
        pr.checks[1].outcome = Checked::Cancelled;
        pr.checks[1].name = "Validation (affected)".into();
        if let Some(bot) = pr.bot.as_mut() {
            bot.findings[0].kind = Found::Comment;
        }
        app.pr_loaded(path, Ok(Some(pr)));
        let shown = screen(&app, 200, 40);
        assert!(
            shown.contains("✗ 1 failed: clippy") && shown.contains("1 cancelled"),
            "{shown}"
        );
        assert!(
            shown.contains("1 comment") && !shown.contains("1 suggestion"),
            "{shown}"
        );
        press(&mut app, KeyCode::Char('2'));
        let shown = screen(&app, 200, 40);
        assert!(
            shown.contains("treetop reads the logs of the first 5 failed checks"),
            "a log not read says so, not that it could not be:\n{shown}"
        );
        press(&mut app, KeyCode::Char('j'));
        let shown = screen(&app, 200, 40);
        assert!(
            shown.contains("Validation (affected) was cancelled."),
            "{shown}"
        );
    }

    #[test]
    fn a_draft_and_an_unread_thread_list_say_so() {
        let mut app = app();
        let path = std::path::PathBuf::from("/pool/4/app");
        while app.current().is_none_or(|t| t.path != path) {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('v'));
        let mut pr = pr();
        pr.draft = true;
        pr.threads_error = Some("HTTP 502".into());
        app.pr_loaded(path, Ok(Some(pr)));
        let shown = screen(&app, 200, 40);
        assert!(shown.contains("a draft, not ready for review"), "{shown}");
        assert!(
            shown.contains("review threads could not be read: HTTP 502"),
            "a failed read is not an empty one:\n{shown}"
        );
        press(&mut app, KeyCode::Char('3'));
        let shown = screen(&app, 200, 40);
        assert!(
            shown.contains("review threads could not be read: HTTP 502"),
            "{shown}"
        );
    }
}

#[cfg(test)]
mod window_tests {
    use super::*;
    use crate::inbox::tests::app;
    use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    fn drawn(app: &App, width: u16, height: u16) -> Buffer {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, app, None, None, None))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn lines(buffer: &Buffer) -> Vec<String> {
        let area = buffer.area;
        (0..area.height)
            .map(|y| (0..area.width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    /// The app with the cursor on tree `name`.
    fn on(name: &str) -> App {
        let mut app = app();
        app.select_tree(name);
        assert_eq!(app.current().map(|t| t.name.as_str()), Some(name));
        app
    }

    #[test]
    fn the_window_under_the_table_shows_the_tree_under_the_cursor_and_its_info() {
        let mut app = on("4");
        app.tab = WindowTab::Info;
        let shown = lines(&drawn(&app, 200, 50));
        let title = shown
            .iter()
            .position(|l| l.starts_with("┌ Tree 4 · me/anb-41-inbox-rail "))
            .unwrap_or_else(|| panic!("the window's title:\n{}", shown.join("\n")));
        let table_ends = shown.iter().position(|l| l.starts_with('└')).unwrap();
        assert!(title > table_ends, "the window is under the table");
        // The info tab holds the lines the detail pane held.
        for label in ["agents", "ports", "processes"] {
            assert!(
                shown[title..]
                    .iter()
                    .any(|l| l.starts_with(&format!("│ {label} "))),
                "{label} in the info:\n{}",
                shown.join("\n")
            );
        }
        let mut app = on("2");
        app.tab = WindowTab::Info;
        let shown = lines(&drawn(&app, 200, 50));
        assert!(
            shown
                .iter()
                .any(|l| l.starts_with("┌ Tree 2 · me/help-columns "))
        );
    }

    #[test]
    fn a_cold_agent_is_left_closed_with_how_to_start_it() {
        let shown = lines(&drawn(&on("9"), 200, 50)).join("\n");
        assert!(
            shown.contains("port-menu is not running; Enter starts it again"),
            "{shown}"
        );
    }

    #[test]
    fn the_window_with_the_keys_has_its_border_in_the_title_colour() {
        let mut app = on("4");
        let corner = |app: &App, title: &str| {
            let buffer = drawn(app, 200, 50);
            let row = lines(&buffer)
                .iter()
                .position(|l| l.starts_with(title))
                .unwrap();
            buffer[(0, u16::try_from(row).unwrap())].fg
        };
        assert_eq!(corner(&app, "┌ Trees "), theme().title);
        assert_eq!(corner(&app, "┌ Tree 4 "), theme().border);
        app.is_window_focused = true;
        assert_eq!(corner(&app, "┌ Trees "), theme().border);
        assert_eq!(corner(&app, "┌ Tree 4 "), theme().title);
        // The inbox with the keys is the one window marked.
        app.is_window_focused = false;
        app.rail = Some(0);
        assert_eq!(corner(&app, "┌ Trees "), theme().border);
        assert_eq!(corner(&app, "┌ Tree 4 "), theme().border);
    }

    #[test]
    fn a_click_on_a_tab_shows_it() {
        let mut app = on("4");
        let shown = lines(&drawn(&app, 200, 50));
        let row = shown
            .iter()
            .position(|l| l.starts_with("│ chat ") && l.contains(" info "))
            .unwrap();
        let column = shown[row]
            .find(" info ")
            .map(|b| shown[row][..b].chars().count())
            .unwrap();
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: u16::try_from(column + 1).unwrap(),
            row: u16::try_from(row).unwrap(),
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            click(&mut app, Rect::new(0, 0, 200, 50), mouse),
            Some(Outcome::Continue)
        );
        assert_eq!(app.tab, WindowTab::Info);
    }

    #[test]
    fn the_table_makes_room_for_the_inbox_and_the_window_for_a_chat() {
        let app = on("4");
        let shown = lines(&drawn(&app, 200, 50));
        // The inbox beside four trees shows every entry and its keys, though
        // the trees alone would need fewer rows.
        for text in ["Done: opened PR #31", "e inbox  E hide"] {
            assert!(
                shown.iter().any(|l| l.contains(text)),
                "{text} in the inbox:\n{}",
                shown.join("\n")
            );
        }
        // The window under the table takes the rest, down to the bar.
        let window = shown
            .iter()
            .position(|l| l.starts_with("┌ Tree 4 "))
            .unwrap();
        assert!(shown[window - 1].starts_with('└'), "{}", shown.join("\n"));
        assert!(shown[48].starts_with('└'), "{}", shown.join("\n"));
        // Too short for both, the table takes the screen, and a tree's chat
        // floats in a popup instead.
        let short = lines(&drawn(&app, 200, 12));
        assert!(
            !short.iter().any(|l| l.contains("┌ Tree 4 ")),
            "{}",
            short.join("\n")
        );
        assert_eq!(window_chat_area(&app, Rect::new(0, 0, 200, 12)), None);
    }

    #[test]
    fn a_tree_chat_floats_only_while_it_has_the_keys_where_the_window_cannot_show() {
        let mut app = on("4");
        let short = Rect::new(0, 0, 80, 10);
        let tall = Rect::new(0, 0, 200, 50);
        // Shown in the window where there is one, keys or not.
        assert!(!floats(&app, false, tall));
        // With nowhere to show it and the keys elsewhere, it stays hidden
        // rather than cover what has them: the table on a short terminal,
        // the Issues screen or its picker.
        assert!(!floats(&app, false, short));
        app.is_issues = true;
        assert!(!floats(&app, false, tall));
        // Given the keys there, it floats in a popup so it is seen.
        app.is_window_focused = true;
        assert!(floats(&app, false, tall));
        app.is_issues = false;
        assert!(floats(&app, false, short));
        // The assistant's and scratch chats always float.
        app.is_window_focused = false;
        assert!(floats(&app, true, tall));
    }

    #[test]
    fn the_run_and_diff_tabs_draw_in_the_window() {
        let mut app = on("4");
        app.runs.insert(
            "/pool/4/app".into(),
            crate::keel::Run {
                tree: "/pool/4/app".into(),
                playbook: "issue".into(),
                goal: "the inbox rail".into(),
                state: "active".into(),
                steps: ["Audit it", "Build it", "Verify it"]
                    .iter()
                    .zip(["done", "doing", "pending"])
                    .map(|(title, state)| crate::keel::Step {
                        title: (*title).into(),
                        state: state.into(),
                    })
                    .collect(),
                current: Some(2),
                attention: 0,
                stale: false,
            },
        );
        app.tab = WindowTab::Run;
        let buffer = drawn(&app, 200, 50);
        let shown = lines(&buffer);
        assert!(
            shown.iter().any(|l| l.contains(" run 2/3 ")),
            "the tab says how far"
        );
        let doing = shown.iter().position(|l| l.contains("◌ Build it")).unwrap();
        assert!(shown[doing - 1].contains("✓ Audit it"));
        assert!(shown[doing + 1].contains("· Verify it"));
        let x = u16::try_from(shown[doing].find("Build").unwrap()).unwrap();
        assert_eq!(
            buffer[(x, u16::try_from(doing).unwrap())].bg,
            theme().cursor.bg.unwrap_or_default(),
            "the step under way is picked out"
        );

        app.handle_key(ratatui::crossterm::event::KeyEvent::from(
            ratatui::crossterm::event::KeyCode::Char('d'),
        ));
        let shown = lines(&drawn(&app, 200, 50));
        let title = shown
            .iter()
            .position(|l| l.starts_with("┌ Tree 4 "))
            .unwrap();
        assert!(
            shown[title + 2].contains(" diff ") && shown[title + 2].contains("tree 4"),
            "the diff's bar is in the window, under its tabs:\n{}",
            shown.join("\n")
        );
        assert!(
            shown[title + 3..]
                .iter()
                .any(|l| l.contains("reading the diff...")),
            "{}",
            shown.join("\n")
        );
    }

    #[test]
    fn on_a_terminal_too_short_for_the_window_the_diff_and_pr_float_in_a_popup() {
        let press = |app: &mut App, c: char| {
            app.handle_key(ratatui::crossterm::event::KeyEvent::from(
                ratatui::crossterm::event::KeyCode::Char(c),
            ));
        };
        let mut app = on("4");
        press(&mut app, 'd');
        let shown = lines(&drawn(&app, 100, 9)).join("\n");
        assert!(
            shown.contains("reading the diff..."),
            "the diff is seen:\n{shown}"
        );
        press(&mut app, 'q');
        press(&mut app, 'v');
        let shown = lines(&drawn(&app, 100, 9)).join("\n");
        assert!(
            shown.contains("reading the PR..."),
            "the PR is seen:\n{shown}"
        );
    }

    #[test]
    fn a_double_shows_two_windows_marked_and_one_at_a_time_when_narrow() {
        let mut app = on("4");
        app.handle_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char('\\'),
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        ));
        let cursor = app.window_tree().unwrap().name.clone();
        let shown = lines(&drawn(&app, 200, 50));
        let titles = shown
            .iter()
            .find(|l| l.starts_with("┌ ◧ Tree 4 "))
            .unwrap_or_else(|| panic!("{}", shown.join("\n")));
        assert!(
            titles.contains(&format!("┌ ◨ Tree {cursor} ")),
            "both side by side: {titles}"
        );
        let row = |name: &str| {
            shown
                .iter()
                .find(|l| l.starts_with(&format!("│  {name}")))
                .cloned()
                .unwrap_or_default()
        };
        assert!(row("4◧").contains("leased"), "{}", shown.join("\n"));
        // A tree shown in both windows carries both marks.
        app.select_tree("4");
        let shown = lines(&drawn(&app, 200, 50));
        assert!(
            shown.iter().any(|l| l.starts_with("│  4◧◨")),
            "{}",
            shown.join("\n")
        );
        app.select_tree(&cursor);
        assert!(row(&format!("{cursor}◨")).contains("leased"));
        // The other window's diff reads once it has the keys, and says so.
        if let Some(other) = app.other.as_mut() {
            other.tab = WindowTab::Diff;
        }
        let shown = lines(&drawn(&app, 200, 50)).join("\n");
        assert!(
            shown.contains("the diff reads once this window has the keys"),
            "{shown}"
        );
        // Too narrow for both, the window the cursor drives shows alone.
        let shown = lines(&drawn(&app, 120, 50)).join("\n");
        assert!(
            shown.contains(&format!("┌ ◨ 2 of 2 · Tree {cursor} ")),
            "{shown}"
        );
        assert!(!shown.contains("┌ ◧"), "{shown}");
    }

    #[test]
    fn the_main_checkouts_heading_carries_its_windows_mark() {
        let mut app = on("4");
        let mut main = crate::pool::main_tree(std::path::Path::new("/pool/main"));
        main.branch = Some("main".into());
        app.base = Some("main".into());
        app.trees.insert(0, main);
        app.selected = 0;
        assert!(
            app.main_under_cursor().is_some(),
            "the cursor is on its heading"
        );
        app.handle_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char('\\'),
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        ));
        let shown = lines(&drawn(&app, 200, 50));
        let heading = shown
            .iter()
            .find(|l| l.contains("main checkout") && l.starts_with("│▾"))
            .unwrap_or_else(|| panic!("{}", shown.join("\n")));
        assert!(heading.contains('◧'), "{heading}");
    }

    #[test]
    fn the_two_windows_are_the_same_width_so_one_chat_size_fits_both() {
        let mut app = on("4");
        app.handle_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char('\\'),
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        ));
        for width in [200, 201] {
            let (cursor, other) = pane_areas(&app, Rect::new(0, 0, width, 30));
            let other = other.unwrap();
            assert_eq!(cursor.width, other.width, "at {width} columns");
            assert!(cursor.right().max(other.right()) <= width);
        }
    }

    #[test]
    fn a_new_agents_form_and_its_start_take_the_window_the_cursor_drives() {
        let mut app = on("4");
        app.handle_key(ratatui::crossterm::event::KeyEvent::from(
            ratatui::crossterm::event::KeyCode::Char('N'),
        ));
        let shown = lines(&drawn(&app, 200, 50));
        let window = shown
            .iter()
            .position(|l| l.starts_with("┌ New agent "))
            .unwrap_or_else(|| panic!("{}", shown.join("\n")));
        let table_ends = shown.iter().position(|l| l.starts_with('└')).unwrap();
        assert!(window > table_ends, "under the table, not over it");
        assert!(
            shown[window..]
                .iter()
                .any(|l| l.contains("New agent in a fresh tree")),
            "{}",
            shown.join("\n")
        );
        // Once sent, the window lists its steps as they end.
        app.prompt = None;
        let key = std::path::PathBuf::from("(new tree 1)");
        app.opening = Some(Opening {
            agent: "fix login".into(),
            tree: "new".into(),
            key: Some(key.clone()),
            failed: None,
            leases: true,
            side: None,
        });
        app.job_step(key.clone(), "lease");
        app.job_step(key.clone(), "branch");
        let shown = lines(&drawn(&app, 200, 50)).join("\n");
        for seen in [
            "Starting fix login",
            "✓ lease a tree",
            "- check it is a clean worktree  not needed here",
            "✓ name its branch",
            "◌ write treetop.json",
            "· start the agent",
            "Esc hides this; the start carries on",
        ] {
            assert!(shown.contains(seen), "{seen}:\n{shown}");
        }
        if let Some(opening) = app.opening.as_mut() {
            opening.failed = Some("record: disk full".into());
        }
        let shown = lines(&drawn(&app, 200, 50)).join("\n");
        assert!(
            shown.contains("✗ write treetop.json  record: disk full"),
            "{shown}"
        );
        assert!(shown.contains("Esc closes this"), "{shown}");
        // In a tree already held nothing is leased, branched or recorded,
        // so only the agent's own steps are listed.
        app.opening = Some(Opening {
            agent: "add tests".into(),
            tree: "4".into(),
            key: Some("/pool/4/app".into()),
            failed: None,
            leases: false,
            side: None,
        });
        let shown = lines(&drawn(&app, 200, 50)).join("\n");
        assert!(shown.contains("◌ start the agent"), "{shown}");
        assert!(!shown.contains("lease a tree"), "{shown}");
    }

    #[test]
    fn a_prompt_opened_over_a_start_is_drawn_and_a_double_keeps_its_other_tree() {
        let mut app = on("4");
        app.handle_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char('\\'),
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        ));
        app.handle_key(ratatui::crossterm::event::KeyEvent::from(
            ratatui::crossterm::event::KeyCode::Char('N'),
        ));
        let shown = lines(&drawn(&app, 200, 50));
        let titles = shown
            .iter()
            .find(|l| l.contains("New agent"))
            .unwrap_or_else(|| panic!("{}", shown.join("\n")));
        assert!(
            titles.contains("┌ ◧ Tree 4 "),
            "the other window keeps its tree: {titles}"
        );
        app.prompt = None;
        app.opening = Some(Opening {
            agent: "fix login".into(),
            tree: "new".into(),
            key: Some("(new tree 1)".into()),
            failed: None,
            leases: true,
            side: None,
        });
        app.handle_key(ratatui::crossterm::event::KeyEvent::from(
            ratatui::crossterm::event::KeyCode::Char(':'),
        ));
        assert!(app.prompt.is_some());
        let shown = lines(&drawn(&app, 200, 50)).join("\n");
        assert!(shown.contains("Ask the assistant"), "{shown}");
    }

    #[test]
    fn a_start_stays_in_the_window_it_was_asked_from_and_the_form_in_its_borders() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let ctrl = |app: &mut App, c: char| {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        };
        let mut app = on("4");
        ctrl(&mut app, '\\');
        app.handle_key(KeyEvent::from(KeyCode::Char('N')));
        app.handle_key(KeyEvent::from(KeyCode::Char('x')));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.window_start().is_some());
        let started_on = app.side;
        // The keys go round to the other window and back; the start stays.
        for _ in 0..2 {
            ctrl(&mut app, 'q');
            let shown = lines(&drawn(&app, 200, 50));
            let titles = shown
                .iter()
                .find(|l| l.contains("New agent"))
                .unwrap_or_else(|| panic!("{}", shown.join("\n")));
            let mark = SIDES[started_on];
            assert!(titles.contains(&format!("┌ {mark} New agent ")), "{titles}");
        }
        // Esc reaches the window with the keys, not the start.
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.window_start().is_some(), "the start is still shown");
        // On a short terminal the form keeps inside its window's border.
        let mut app = on("4");
        app.handle_key(KeyEvent::from(KeyCode::Char('N')));
        let shown = lines(&drawn(&app, 80, 20));
        assert!(
            shown.iter().any(|l| l.starts_with("┌ New agent ")),
            "{}",
            shown.join("\n")
        );
    }
}
