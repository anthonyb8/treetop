//! Drawing, htop style: the pool table, a detail pane for the tree under the
//! cursor, and a key bar with the summary. A chat, a diff or the compose box
//! floats over them in a popup, with the table dimmed round it, so treetop
//! never leaves the screen. Every colour comes from `theme`.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};

use crate::agents::{Agent, Status};
use crate::app::{App, DiffView, Item, JobState, Opening, Pane, Pending, Picker, Review, Screen};
use crate::chat::Chat;
use crate::coordinator;
use crate::diff::{Change, FileDiff, Layout as DiffLayout, Row as DiffRow, Side, ViewRow};
use crate::dispatch::{Prompt, Purpose, Target};
use crate::pool::{Tree, Work};
use crate::popup;
use crate::reports::{self, State};
use crate::theme::theme;

/// Most useful first: on a narrow bar the keys at the end are the ones left
/// out.
const KEYS: [(&str, &str); 19] = [
    ("Tab", "inbox"),
    ("n", "ask/dispatch"),
    ("w", "workspace"),
    ("m", "message"),
    ("t", "tree"),
    ("c", "chat"),
    ("d", "diff"),
    ("b", "branch"),
    ("o", "open port"),
    ("a", "no agent"),
    ("Space", "mark"),
    ("r", "return"),
    ("D", "destroy"),
    ("/", "filter"),
    ("L", "log"),
    ("q", "quit"),
    ("^R", "refresh"),
    ("u", "unmark"),
    ("j/k", "move"),
];

/// The inbox's keys, most useful first, as with `KEYS`.
const INBOX_KEYS: [(&str, &str); 10] = [
    ("Enter", "reply"),
    ("c", "chat"),
    ("x", "read"),
    ("n", "ask/dispatch"),
    ("Tab", "trees"),
    ("t", "tree"),
    ("r", "return tree"),
    ("w", "workspace"),
    ("j/k", "move"),
    ("q", "quit"),
];

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

pub fn draw(frame: &mut Frame, app: &App, chat: Option<&Chat>) {
    // The theme's text colour is the base for every cell; widgets below only
    // add accents, so nothing falls back to a theme's dim default.
    frame.render_widget(Block::new().style(fg(theme().text)), frame.area());
    let is_inbox = app.screen == Screen::Inbox;
    let [body, detail, footer] = Layout::vertical([
        Constraint::Fill(1),
        match (is_inbox, app.pane) {
            (true, _) => Constraint::Length(13),
            (false, Pane::Info) => Constraint::Length(6),
            (false, Pane::Log) => Constraint::Fill(1),
        },
        Constraint::Length(1),
    ])
    .areas(frame.area());
    if is_inbox {
        draw_inbox(frame, app, body);
        draw_report(frame, app, detail);
    } else {
        draw_table(frame, app, body);
        match app.pane {
            Pane::Info => frame.render_widget(detail_pane(app), detail),
            Pane::Log => frame.render_widget(log_pane(app, detail.height), detail),
        }
    }
    draw_footer(frame, app, footer);
    if chat.is_some()
        || app.view.is_some()
        || app.prompt.is_some()
        || app.opening.is_some()
        || app.picker.is_some()
    {
        let area = frame.area();
        frame
            .buffer_mut()
            .set_style(area, Style::new().add_modifier(Modifier::DIM));
    }
    if let Some(chat) = chat {
        draw_chat(frame, app, chat);
    } else if let Some(opening) = &app.opening {
        draw_opening(frame, app, opening);
    } else if let Some(view) = &app.view {
        draw_diff_view(frame, app, view);
    } else if let Some(prompt) = &app.prompt {
        draw_prompt(frame, prompt);
    } else if let Some(picker) = &app.picker {
        draw_picker(frame, app, picker);
    } else if let Some(review) = app.reviews.front() {
        draw_review(frame, review, app.reviews.len() - 1);
    } else if let Some(pending) = &app.pending {
        draw_confirm(frame, pending);
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
        let agents = app.trees.iter().map(|t| t.agents.len()).sum::<usize>();
        let waiting = app
            .trees
            .iter()
            .flat_map(|t| &t.agents)
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
        if !app.marked.is_empty() {
            spans.push(Span::raw(format!("  {} marked", app.marked.len())));
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

fn count_cell(value: Option<usize>, color: Color) -> Cell<'static> {
    let muted = fg(theme().muted);
    match value {
        Some(0) => Cell::from("0").style(muted),
        Some(n) => Cell::from(n.to_string()).style(fg(color)),
        None => Cell::from("-").style(muted),
    }
}

/// A git count for one tree: `-` where there is nothing to count (an
/// available tree), `?` where git could not read a held one.
fn work_cell(tree: &Tree, count: fn(Work) -> usize, color: Color) -> Cell<'static> {
    match tree.work {
        None if tree.is_held() => Cell::from("?").style(fg(theme().warning)),
        work => count_cell(work.map(count), color),
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
        None => ("·", Style::new()),
    }
}

/// The first agent in the tree, and how many more. A held tree with none,
/// once agents have been read, says so: it is the one to check on.
fn agent_cell(app: &App, tree: &Tree) -> Cell<'static> {
    let Some(first) = tree.agents.first() else {
        return if tree.is_held() && app.is_agents_read {
            Cell::from("no agent").style(fg(theme().muted))
        } else {
            Cell::from("")
        };
    };
    let (glyph, style) = agent_style(first.status);
    let more = match tree.agents.len() - 1 {
        0 => String::new(),
        n => format!(" +{n}"),
    };
    Cell::from(format!("{glyph} {}{more}", first.name)).style(style)
}

/// The lowest port the tree serves and how many more, `-` for none.
fn ports_cell(tree: &Tree) -> Cell<'static> {
    let ports = tree.ports();
    match ports.as_slice() {
        [] => Cell::from("-").style(fg(theme().muted)),
        [first] => Cell::from(format!(":{first}")).style(fg(theme().info)),
        [first, rest @ ..] => {
            Cell::from(format!(":{first} +{}", rest.len())).style(fg(theme().info))
        }
    }
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
    let procs = (!tree.processes.is_empty() || tree.is_held()).then_some(tree.processes.len());
    Row::new(vec![
        Cell::from(mark),
        Cell::from(tree.name.clone()),
        status_cell(app, tree),
        branch,
        agent_cell(app, tree),
        Cell::from(tree.holder.clone().unwrap_or_default()),
        count_cell(procs, t.warning),
        ports_cell(tree),
        work_cell(tree, |w| w.changed, t.danger),
        work_cell(tree, |w| w.unpushed, t.special),
    ])
    .style(if app.is_cached {
        Style::new().add_modifier(Modifier::ITALIC)
    } else {
        Style::new()
    })
}

fn draw_table(frame: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let header = Row::new([
        "", "#", "STATUS", "BRANCH", "AGENT", "HOLDER", "PROCS", "PORTS", "CHANGED", "UNPUSHED",
    ])
    .style(t.header);
    let rows: Vec<Row> = app
        .visible()
        .into_iter()
        .map(|tree| row(app, tree))
        .collect();
    let widths = [
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Length(13),
        Constraint::Fill(1),
        Constraint::Length(22),
        Constraint::Length(14),
        Constraint::Length(6),
        Constraint::Length(11),
        Constraint::Length(8),
        Constraint::Length(9),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(t.cursor);
    let mut state = TableState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn pane_block() -> Block<'static> {
    Block::bordered().border_style(fg(theme().border))
}

fn detail_pane(app: &App) -> Paragraph<'static> {
    let t = theme();
    let Some(tree) = app.current() else {
        let text = if app.is_loaded { "no trees match" } else { "" };
        return Paragraph::new(text).block(pane_block());
    };
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
    Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                tree.path.display().to_string(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                since.map(|s| format!("  leased {s}")).unwrap_or_default(),
                fg(t.muted),
            ),
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
    ])
    .block(pane_block())
}

/// Each agent with its status, where it was seen and its pid.
fn agents_line(app: &App, tree: &Tree) -> Line<'static> {
    let t = theme();
    let mut spans = vec![Span::styled("agents    ", fg(t.muted))];
    if tree.agents.is_empty() {
        let text = if app.is_agents_read {
            "none"
        } else {
            "reading..."
        };
        spans.push(Span::raw(text));
        return Line::from(spans);
    }
    for (i, agent) in tree.agents.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        spans.extend(agent_spans(agent));
    }
    Line::from(spans)
}

fn agent_spans(agent: &Agent) -> Vec<Span<'static>> {
    let (glyph, style) = agent_style(agent.status);
    let mut about = vec![
        agent.source.label().to_string(),
        format!("pid {}", agent.pid),
    ];
    if let Some(status) = agent.status {
        about.insert(0, status.label().to_string());
    }
    if agent.is_background {
        about.push("background".into());
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
    if app.is_orphans_only {
        narrowing.push(Span::styled(" no agent only", fg(t.accent)));
    }
    if !app.filter.is_empty() {
        narrowing.push(Span::styled(
            format!(" filter: {}", app.filter),
            fg(t.accent),
        ));
    }
    let used = u16::try_from(narrowing.iter().map(Span::width).sum::<usize>()).unwrap_or(width);
    let keys: &[(&str, &str)] = match app.screen {
        Screen::Trees => &KEYS,
        Screen::Inbox => &INBOX_KEYS,
    };
    let mut spans = fitting_keys(keys, width.saturating_sub(used));
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

/// How a report's state reads: what needs you is the loud one, what is new
/// stands out, and what has been read steps back.
fn report_style(item: &Item) -> (&'static str, Style) {
    let t = theme();
    let style = match item.state {
        State::Blocked => fg(t.warning).add_modifier(Modifier::BOLD),
        State::Done => fg(t.success),
        State::Working => fg(t.info),
        State::Stopped => fg(t.muted),
    };
    let glyph = match item.state {
        State::Blocked => "●",
        State::Done => "✓",
        State::Working => "·",
        State::Stopped => "■",
    };
    if item.is_read {
        (glyph, fg(t.muted))
    } else {
        (glyph, style)
    }
}

/// The inbox: one row per agent's latest report, what needs you first.
fn draw_inbox(frame: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let items = app.inbox();
    let header = Row::new(["", "WHERE", "AGENT", "STATE", "REPORT", "AGE"]).style(t.header);
    let rows: Vec<Row> = items
        .iter()
        .map(|item| {
            let (glyph, style) = report_style(item);
            let text_style = if item.is_read {
                fg(t.muted)
            } else {
                Style::new()
            };
            let first_line = item.text().lines().next().unwrap_or("").to_string();
            Row::new(vec![
                Cell::from(glyph).style(style),
                Cell::from(item.place()),
                Cell::from(item.report.name.clone()).style(text_style),
                Cell::from(item.state.label()).style(style),
                Cell::from(first_line).style(text_style),
                Cell::from(reports::age(item.report.updated)).style(fg(t.muted)),
            ])
        })
        .collect();
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(" No reports yet. n dispatches an agent; what it reports lands here.")
                .style(fg(t.muted)),
            area,
        );
        return;
    }
    // The report is what the inbox is for, so the agent's name gives way to
    // it on a narrow screen.
    let widths = [
        Constraint::Length(1),
        Constraint::Length(5),
        Constraint::Length((area.width / 5).clamp(12, 28)),
        Constraint::Length(9),
        Constraint::Fill(1),
        Constraint::Length(4),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(t.cursor);
    let mut state = TableState::default().with_selected(Some(app.inbox_selected()));
    frame.render_stateful_widget(table, area, &mut state);
}

/// The report under the cursor in full: where it works, what it says, and
/// its latest steps.
fn draw_report(frame: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(item) = app.current_item() else {
        frame.render_widget(Paragraph::new("").block(pane_block()), area);
        return;
    };
    let block = pane_block().title(format!(" {} ", item.report.name));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut place = vec![Span::styled(
        match item.tree {
            Some(tree) if tree.is_main => "main checkout".to_string(),
            Some(tree) => format!("tree {}", tree.name),
            None => "outside the pool".to_string(),
        },
        Style::new().add_modifier(Modifier::BOLD),
    )];
    if let Some(tree) = item.tree {
        place.push(Span::raw(format!(
            "  {}",
            tree.branch.as_deref().unwrap_or("(detached)")
        )));
        if let Some(work) = tree.work {
            place.push(Span::styled(
                format!("  {} changed", work.changed),
                fg(if work.changed > 0 { t.danger } else { t.muted }),
            ));
            place.push(Span::styled(
                format!("  {} unpushed", work.unpushed),
                fg(if work.unpushed > 0 {
                    t.special
                } else {
                    t.muted
                }),
            ));
        }
        if let Some(port) = tree.ports().first() {
            place.push(Span::styled(format!("  :{port}"), fg(t.info)));
        }
    }
    if let Some(dir) = &item.report.dir {
        place.push(Span::styled(format!("  {}", dir.display()), fg(t.muted)));
    }
    let live = match item.agent {
        Some(agent) => format!("running, pid {}", agent.pid),
        None => "not running; c or a reply starts it again".into(),
    };
    place.push(Span::styled(
        format!("  job {} · {live}", item.report.job),
        fg(t.muted),
    ));
    let (glyph, style) = report_style(&item);
    let mut said = vec![Line::from(place)];
    for (i, text) in item.text().lines().enumerate() {
        let lead = if i == 0 {
            Span::styled(format!("{glyph} {} ", item.state.label()), style)
        } else {
            Span::raw("  ")
        };
        said.push(Line::from(vec![lead, Span::raw(text.to_string())]));
    }
    let said_height = u16::try_from(wrapped_height(&said, inner.width) + 1)
        .unwrap_or(u16::MAX)
        .min(inner.height.saturating_sub(3).max(2));
    let said = Paragraph::new(said).wrap(Wrap { trim: false });
    let [said_area, steps_area] =
        Layout::vertical([Constraint::Length(said_height), Constraint::Fill(1)]).areas(inner);
    frame.render_widget(said, said_area);
    let shown = usize::from(steps_area.height.saturating_sub(1));
    let steps = &item.report.steps[item.report.steps.len().saturating_sub(shown)..];
    let mut lines = vec![Line::styled("steps", fg(t.muted))];
    lines.extend(steps.iter().map(|step| {
        let at = step.at.map_or_else(|| "--:--".into(), reports::clock);
        let state = step.state.map_or("", State::label);
        Line::from(vec![
            Span::styled(format!("{at} "), fg(t.muted)),
            Span::styled(format!("{state:<9} "), fg(t.muted)),
            Span::raw(step.detail.replace('\n', " ")),
        ])
    }));
    frame.render_widget(Paragraph::new(lines), steps_area);
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
    let rows: Vec<Row> = picker
        .choices
        .iter()
        .map(|choice| {
            let is_here = choice.checkout == app.checkout;
            let held = match choice.held {
                Some(n) => format!("{n} held"),
                None => "not listed yet".into(),
            };
            Row::new(vec![
                Cell::from(if is_here { "●" } else { " " }).style(fg(t.accent)),
                Cell::from(choice.name.clone()).style(Style::new().add_modifier(Modifier::BOLD)),
                Cell::from(held).style(fg(t.info)),
                Cell::from(choice.checkout.display().to_string()).style(fg(t.muted)),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(1),
        Constraint::Length(20),
        Constraint::Length(14),
        Constraint::Fill(1),
    ];
    let table = Table::new(rows, widths).row_highlight_style(t.cursor);
    let mut state = TableState::default().with_selected(Some(picker.selected));
    frame.render_stateful_widget(table, inner, &mut state);
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
fn draw_prompt(frame: &mut Frame, prompt: &Prompt) {
    let t = theme();
    let screen = frame.area();
    let width = screen.width.saturating_sub(4).min(116);
    let text_width = usize::from(width.saturating_sub(4));
    let rows = wrap_chars(&prompt.text, text_width);
    let text_height = u16::try_from(rows.len())
        .unwrap_or(u16::MAX)
        .clamp(COMPOSE_ROWS.0, COMPOSE_ROWS.1);
    let targets = match &prompt.purpose {
        Purpose::Dispatch { targets, at } => Some((targets, *at)),
        Purpose::Message { .. } | Purpose::Rename { .. } => None,
    };
    // Borders, then the targets, the branch and a blank line when there are
    // targets.
    let chrome = if targets.is_some() { 5 } else { 2 };
    let area = popup::centered(screen, width, text_height + chrome);
    let (title, placeholder) = match (prompt.target(), &prompt.purpose) {
        (Some(Target::Coordinator), _) => (
            " Ask the coordinator ".to_string(),
            "ask about the agents or give it work; send nothing to just open its chat",
        ),
        (Some(_), _) => (
            " New agent ".to_string(),
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
        (None, Purpose::Rename { tree }) => (
            format!(" Rename the branch of tree {} ", tree.name),
            "the branch's new name",
        ),
    };
    let mut keys = vec![("Enter", "send"), ("Esc", "cancel")];
    if !matches!(prompt.purpose, Purpose::Rename { .. }) && !prompt.is_editing_branch {
        keys.insert(1, ("Alt+Enter", "new line"));
    }
    if prompt.names_branch() {
        keys.insert(0, ("↑↓", "branch"));
    }
    if targets.is_some() {
        keys.insert(0, ("Tab", "to"));
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
    if let Some((targets, at)) = targets {
        let mut spans = vec![Span::styled("to ", fg(t.muted))];
        for (i, target) in targets.iter().enumerate() {
            let label = format!(" {} ", target.label());
            spans.push(if i == at {
                Span::styled(label, t.badge)
            } else {
                Span::styled(label, fg(t.muted))
            });
            spans.push(Span::raw(" "));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        let branch_row = Rect::new(inner.x, inner.y + 1, inner.width, 1);
        frame.render_widget(Paragraph::new(branch_line(prompt)), branch_row);
        if prompt.is_editing_branch {
            let typed = prompt.branch.as_deref().map_or(0, str::len);
            let at = u16::try_from("branch ".len() + typed).unwrap_or(u16::MAX);
            frame.set_cursor_position(Position::new(
                (branch_row.x + at).min(branch_row.right().saturating_sub(1)),
                branch_row.y,
            ));
        }
        text_area = Rect::new(
            inner.x,
            inner.y + 3,
            inner.width,
            inner.height.saturating_sub(3),
        );
    }
    if prompt.text.is_empty() {
        frame.render_widget(
            Paragraph::new(placeholder)
                .wrap(Wrap { trim: false })
                .style(fg(t.muted).add_modifier(Modifier::ITALIC)),
            text_area,
        );
        if !prompt.is_editing_branch {
            frame.set_cursor_position(Position::new(text_area.x, text_area.y));
        }
        return;
    }
    // The end of a long text stays in view, since that is where it is typed.
    let shown = usize::from(text_area.height);
    let first = rows.len().saturating_sub(shown);
    let lines: Vec<Line> = rows[first..].iter().map(|r| Line::raw(r.clone())).collect();
    frame.render_widget(Paragraph::new(lines), text_area);
    if prompt.is_editing_branch {
        return;
    }
    let last = rows.last().map_or(0, |row| row.chars().count());
    let cursor_row = u16::try_from(rows.len() - 1 - first).unwrap_or(0);
    frame.set_cursor_position(Position::new(
        text_area.x + u16::try_from(last).unwrap_or(0),
        text_area.y + cursor_row,
    ));
}

/// The compose box's branch line: the branch a new agent will start on,
/// edited or following the task, or the branch a held tree keeps.
fn branch_line(prompt: &Prompt) -> Line<'static> {
    let t = theme();
    let label = Span::styled("branch ", fg(t.muted));
    let kept = |branch: Option<&String>| {
        Line::from(vec![
            label.clone(),
            Span::styled(
                format!("{} (kept)", branch.map_or("(detached)", String::as_str)),
                fg(t.muted),
            ),
        ])
    };
    match prompt.target() {
        Some(Target::Tree(tree) | Target::InPlace(tree)) if !prompt.names_branch() => {
            kept(tree.branch.as_ref())
        }
        Some(Target::NewTree | Target::Tree(_)) => {
            let name = if prompt.is_editing_branch {
                Span::styled(
                    prompt.branch.clone().unwrap_or_default(),
                    fg(t.accent).add_modifier(Modifier::BOLD),
                )
            } else {
                match prompt.branch_name() {
                    Some(name) => Span::raw(name),
                    None => Span::styled(
                        "named from the task",
                        fg(t.muted).add_modifier(Modifier::ITALIC),
                    ),
                }
            };
            Line::from(vec![label, name])
        }
        _ => Line::default(),
    }
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

/// What returning a tree throws away, as its confirm line says it.
fn risk(tree: &Tree) -> Vec<Span<'static>> {
    let t = theme();
    let mut spans = Vec::new();
    match tree.work {
        None => spans.push(Span::styled(
            "  unknown changes: git cannot read it",
            fg(t.warning),
        )),
        Some(w) if w.changed + w.unpushed > 0 => spans.push(Span::styled(
            format!("  {} changed, {} unpushed", w.changed, w.unpushed),
            fg(t.danger).add_modifier(Modifier::BOLD),
        )),
        Some(_) => spans.push(Span::styled("  clean", fg(t.success))),
    }
    if !tree.processes.is_empty() {
        spans.push(Span::styled(
            format!(", {} process(es) running", tree.processes.len()),
            fg(t.warning),
        ));
    }
    spans
}

fn draw_confirm(frame: &mut Frame, pending: &Pending) {
    let mut lines: Vec<Line> = pending
        .trees
        .iter()
        .map(|t| {
            let mut spans = vec![Span::raw(format!(
                " {:>3}  {}",
                t.name,
                t.branch.as_deref().unwrap_or("(detached)")
            ))];
            spans.extend(risk(t));
            Line::from(spans)
        })
        .collect();
    lines.push(Line::raw(""));
    lines.push(Line::raw(
        " Uncommitted changes are discarded and running processes stopped.",
    ));
    lines.push(Line::raw(
        " y returns them to the pool; any other key cancels.",
    ));
    let title = format!(" Return {} tree(s)? ", pending.trees.len());
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = popup::centered(frame.area(), 80, height);
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
    let skip = usize::from(hscroll);
    let chars: Vec<char> = side.text.chars().skip(skip).collect();
    let (from, to) = side.emphasis.map_or((0, 0), |(from, to)| {
        (
            from.saturating_sub(skip).min(chars.len()),
            to.saturating_sub(skip).min(chars.len()),
        )
    });
    let piece = |range: std::ops::Range<usize>| chars[range].iter().collect::<String>();
    let on_change = side.is_change.then_some(palette.change_text).flatten();
    let spans = vec![
        Span::styled(
            format!("{:>number_width$} ", side.number),
            fg(on_change.unwrap_or(palette.number)),
        ),
        Span::raw(piece(0..from)),
        Span::styled(piece(from..to), fg(palette.word_text).bg(word)),
        Span::raw(piece(to..chars.len())),
    ];
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

/// The side-by-side diff of one tree against its base, in a popup.
fn draw_diff_view(frame: &mut Frame, app: &App, view: &DiffView) {
    let t = theme();
    let area = popup::area(frame.area());
    // The keys sit in a bar on the popup's last row, drawn like a file header.
    let [body, key_row] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)])
        .areas(popup::inner(frame.area()));
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
    frame.render_widget(Clear, area);
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
    frame.render_widget(popup_block(Line::from(title)), area);
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

/// An agent's chat in a popup over treetop, under a title naming its branch,
/// tree and agent, with the keys out at the right of the same line. The branch comes from
/// the latest listing, so it follows the agent if it switches branches.
fn draw_chat(frame: &mut Frame, app: &App, chat: &Chat) {
    let body = chat_frame(frame, app, &chat.agent, &chat.tree);
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
    let body = chat_frame(frame, app, &opening.agent, &opening.tree);
    let note = if opening.tree == "new" {
        "leasing a tree for the agent...".to_string()
    } else {
        opening_note(&opening.agent)
    };
    draw_note(frame, body, &note);
}

fn opening_note(agent: &str) -> String {
    if agent == coordinator::NAME {
        "opening the coordinator...".into()
    } else {
        format!("opening {agent}...")
    }
}

/// A chat's popup: its frame and title, cleared, with the keys out at the
/// right of the title. Returns where the chat's screen goes.
fn chat_frame(frame: &mut Frame, app: &App, agent: &str, tree: &str) -> Rect {
    let t = theme();
    let area = popup::area(frame.area());
    let title = if agent == coordinator::NAME {
        vec![Span::styled(" coordinator ", t.badge)]
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
    let keys = Line::from(key_bar(&[("^Q", "back to treetop"), ("^\\", "next chat")]));
    frame.render_widget(Clear, area);
    frame.render_widget(
        popup_block(Line::from(title)).title_top(keys.right_aligned()),
        area,
    );
    popup::inner(frame.area())
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

/// Every finished job's output, newest at the bottom, scrolled up by
/// `log_scroll` lines.
fn log_pane(app: &App, height: u16) -> Paragraph<'static> {
    let t = theme();
    let mut lines: Vec<Line> = Vec::new();
    for entry in &app.log {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        let (verdict, color) = if entry.is_ok {
            ("ok", t.success)
        } else {
            ("failed", t.danger)
        };
        lines.push(Line::styled(
            format!("{} tree {}: {verdict}", entry.kind.noun(), entry.tree),
            fg(color).add_modifier(Modifier::BOLD),
        ));
        lines.extend(entry.output.lines().map(|l| Line::raw(format!("  {l}"))));
    }
    if lines.is_empty() {
        lines.push(Line::raw("no actions yet"));
    }
    let shown = usize::from(height.saturating_sub(2));
    let top = lines
        .len()
        .saturating_sub(shown)
        .saturating_sub(app.log_scroll);
    Paragraph::new(lines)
        .scroll((u16::try_from(top).unwrap_or(u16::MAX), 0))
        .block(
            pane_block()
                .title(" log ")
                .title_bottom(" PgUp/PgDn scroll   L closes "),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

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
