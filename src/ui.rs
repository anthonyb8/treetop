//! Drawing, htop style: a summary line, the pool table, a detail pane for the
//! tree under the cursor, and a key bar.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};

use crate::app::{App, JobState, Pane, Pending, Review};
use crate::diff::{self, Tone};
use crate::pool::{Tree, Work};

const KEYS: [(&str, &str); 11] = [
    ("Space", "mark"),
    ("u", "unmark"),
    ("/", "filter"),
    ("Enter", "open"),
    ("r", "return"),
    ("D", "destroy"),
    ("j/k", "move"),
    ("Tab", "diff"),
    ("L", "log"),
    ("^R", "refresh"),
    ("q", "quit"),
];

pub fn draw(frame: &mut Frame, app: &App) {
    let [summary, body, detail, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        if app.pane == Pane::Info {
            Constraint::Length(4)
        } else {
            Constraint::Fill(1)
        },
        Constraint::Length(1),
    ])
    .areas(frame.area());

    // White is the base for every cell; widgets below only add accents, so
    // nothing falls back to a terminal theme's default or dim foreground.
    frame.render_widget(
        Block::new().style(Style::new().fg(Color::White)),
        frame.area(),
    );
    frame.render_widget(summary_line(app), summary);
    draw_table(frame, app, body);
    match app.pane {
        Pane::Info => frame.render_widget(detail_pane(app), detail),
        Pane::Diff => frame.render_widget(diff_pane(app), detail),
        Pane::Log => frame.render_widget(log_pane(app, detail.height), detail),
    }
    frame.render_widget(footer_line(app), footer);
    if let Some(review) = app.reviews.front() {
        draw_review(frame, review, app.reviews.len() - 1);
    } else if let Some(pending) = &app.pending {
        draw_confirm(frame, pending);
    }
}

fn summary_line(app: &App) -> Paragraph<'static> {
    if let Some(err) = &app.error {
        return Paragraph::new(Span::styled(err.clone(), Style::new().fg(Color::Red)));
    }
    if !app.is_loaded {
        return Paragraph::new(Line::from(vec![
            Span::styled("treetop", Style::new().add_modifier(Modifier::BOLD)),
            Span::styled("  reading the pool...", Style::new().fg(Color::White)),
        ]));
    }
    let held = app.trees.iter().filter(|t| t.is_held()).count();
    let running = app.trees.iter().filter(|t| !t.processes.is_empty()).count();
    Paragraph::new(Line::from(vec![
        Span::styled("treetop", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(format!("  {} trees  {held} held  ", app.trees.len())),
        Span::styled(format!("{running} running"), Style::new().fg(Color::Yellow)),
        Span::raw(format!("  {} marked", app.marked.len())),
        match app.active_jobs() {
            0 => Span::raw(""),
            n => Span::styled(
                format!("  {n} action(s) running"),
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
        },
        freshness(app),
        if app.is_listing {
            Span::styled("  refreshing...", Style::new().fg(Color::Cyan))
        } else {
            Span::raw("")
        },
    ]))
}

/// How old the pool listing is. Processes and git counts are always within a
/// couple of seconds; this is about leases, which only treehouse knows.
fn freshness(app: &App) -> Span<'static> {
    if app.is_cached {
        return Span::styled("  cached listing", Style::new().fg(Color::Yellow));
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
    match value {
        Some(0) => Cell::from("0").style(Style::new().fg(Color::White)),
        Some(n) => Cell::from(n.to_string()).style(Style::new().fg(color)),
        None => Cell::from("-").style(Style::new().fg(Color::White)),
    }
}

/// A git count for one tree: `-` where there is nothing to count (an
/// available tree), `?` where git could not read a held one.
fn work_cell(tree: &Tree, count: fn(Work) -> usize, color: Color) -> Cell<'static> {
    match tree.work {
        None if tree.is_held() => Cell::from("?").style(Style::new().fg(Color::Yellow)),
        work => count_cell(work.map(count), color),
    }
}

/// The tree's job while one is queued, running, just done or failed, else its
/// pool status.
fn status_cell(app: &App, tree: &Tree) -> Cell<'static> {
    let bold = Modifier::BOLD;
    let (text, style) = match app.jobs.get(&tree.path) {
        Some(JobState::Queued(_)) => ("queued".to_string(), Style::new().fg(Color::Cyan)),
        Some(JobState::Running(kind)) => (
            format!("{}...", kind.running()),
            Style::new().fg(Color::Cyan).add_modifier(bold),
        ),
        Some(JobState::Done(kind, _)) => (
            kind.done().to_string(),
            Style::new().fg(Color::Green).add_modifier(bold),
        ),
        Some(JobState::Failed(_)) => (
            "failed".to_string(),
            Style::new().fg(Color::Red).add_modifier(bold),
        ),
        None => {
            let color = match tree.status.as_str() {
                "leased" => Color::Green,
                "available" => Color::White,
                _ => Color::Red,
            };
            (tree.status.clone(), Style::new().fg(color))
        }
    };
    Cell::from(text).style(style)
}

fn row(app: &App, tree: &Tree) -> Row<'static> {
    let is_here = app.is_here(tree);
    let mark = if app.marked.contains(&tree.path) {
        Span::styled(
            "*",
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )
    } else if is_here {
        Span::styled("@", Style::new().fg(Color::Blue))
    } else {
        Span::raw(" ")
    };
    let branch = match &tree.branch {
        Some(b) => Cell::from(b.clone()),
        None => Cell::from("(detached)").style(Style::new().fg(Color::White)),
    };
    let procs = (!tree.processes.is_empty() || tree.is_held()).then_some(tree.processes.len());
    Row::new(vec![
        Cell::from(mark),
        Cell::from(tree.name.clone()),
        status_cell(app, tree),
        branch,
        Cell::from(tree.holder.clone().unwrap_or_default()),
        count_cell(procs, Color::Yellow),
        work_cell(tree, |w| w.changed, Color::Red),
        work_cell(tree, |w| w.unpushed, Color::Magenta),
    ])
    .style(if app.is_cached {
        Style::new().add_modifier(Modifier::ITALIC)
    } else {
        Style::new()
    })
}

fn draw_table(frame: &mut Frame, app: &App, area: Rect) {
    let header = Row::new([
        "", "#", "STATUS", "BRANCH", "HOLDER", "PROCS", "CHANGED", "UNPUSHED",
    ])
    .style(
        Style::new()
            .fg(Color::White)
            .bg(Color::Blue)
            .add_modifier(Modifier::BOLD),
    );
    let rows: Vec<Row> = app.visible().into_iter().map(|t| row(app, t)).collect();
    let widths = [
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Length(13),
        Constraint::Fill(1),
        Constraint::Length(20),
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Length(9),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn detail_pane(app: &App) -> Paragraph<'static> {
    let block = Block::bordered().border_style(Style::new().fg(Color::White));
    let Some(tree) = app.current() else {
        let text = if app.is_loaded { "no trees match" } else { "" };
        return Paragraph::new(text).block(block);
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
    Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                tree.path.display().to_string(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                since.map(|s| format!("  leased {s}")).unwrap_or_default(),
                Style::new().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("processes ", Style::new().fg(Color::White)),
            Span::raw(procs),
        ]),
    ])
    .block(block)
}

fn footer_line(app: &App) -> Paragraph<'static> {
    if app.is_filtering {
        return Paragraph::new(format!(
            "Filter: {}_   Enter keeps it, Esc clears it",
            app.filter
        ));
    }
    if let Some(message) = &app.message {
        return Paragraph::new(Span::styled(
            message.clone(),
            Style::new().fg(Color::Yellow),
        ));
    }
    let key = Style::new()
        .fg(Color::White)
        .bg(Color::Blue)
        .add_modifier(Modifier::BOLD);
    let mut spans: Vec<Span> = KEYS
        .iter()
        .flat_map(|(k, label)| {
            [
                Span::styled(format!(" {k} "), key),
                Span::raw(format!(" {label}  ")),
            ]
        })
        .collect();
    if !app.filter.is_empty() {
        spans.push(Span::styled(
            format!(" filter: {}", app.filter),
            Style::new().fg(Color::Cyan),
        ));
    }
    Paragraph::new(Line::from(spans))
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

/// What returning a tree throws away, as its confirm line says it.
fn risk(tree: &Tree) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    match tree.work {
        None => spans.push(Span::styled(
            "  unknown changes: git cannot read it",
            Style::new().fg(Color::Yellow),
        )),
        Some(w) if w.changed + w.unpushed > 0 => spans.push(Span::styled(
            format!("  {} changed, {} unpushed", w.changed, w.unpushed),
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Some(_) => spans.push(Span::styled("  clean", Style::new().fg(Color::Green))),
    }
    if !tree.processes.is_empty() {
        spans.push(Span::styled(
            format!(", {} process(es) running", tree.processes.len()),
            Style::new().fg(Color::Yellow),
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
    let area = centered(frame.area(), 80, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(title)
                .border_style(Style::new().fg(Color::Yellow)),
        ),
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
    let area = centered(full, full.width * 4 / 5, height);
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
            .block(
                Block::bordered()
                    .title(title)
                    .title_bottom(keys)
                    .border_style(Style::new().fg(Color::Red)),
            ),
        area,
    );
}

fn tone_style(tone: Tone) -> Style {
    match tone {
        Tone::Heading => Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        Tone::Added => Style::new().fg(Color::Green),
        Tone::Modified => Style::new().fg(Color::Yellow),
        Tone::Deleted => Style::new().fg(Color::Red),
        Tone::Plain => Style::new().fg(Color::White),
        Tone::Note => Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
    }
}

/// The tree under the cursor: its uncommitted files, changed lines and
/// unpushed commits, or why there are none to show.
fn diff_pane(app: &App) -> Paragraph<'static> {
    let mut block = Block::bordered()
        .title_bottom(" Tab closes   PgUp/PgDn scroll ")
        .border_style(Style::new().fg(Color::White));
    let Some(tree) = app.current() else {
        return Paragraph::new("no trees match").block(block);
    };
    block = block.title(format!(
        " diff: tree {} ({}) ",
        tree.name,
        tree.branch.as_deref().unwrap_or("detached")
    ));
    let note = |text: &str| {
        vec![Line::styled(
            text.to_string(),
            Style::new().fg(Color::Yellow),
        )]
    };
    let lines = if !tree.is_held() {
        note("An available tree holds no work: treehouse keeps it clean.")
    } else if tree.work.is_none() {
        note("git cannot read this tree, so there is no diff to show.")
    } else {
        match app.diffs.get(&tree.path) {
            None => note("reading git..."),
            Some(loaded) => match &loaded.diff {
                None => note("git cannot read this tree, so there is no diff to show."),
                Some(d) => diff::lines(d)
                    .into_iter()
                    .map(|l| Line::styled(l.text, tone_style(l.tone)))
                    .collect(),
            },
        }
    };
    Paragraph::new(lines)
        .scroll((app.diff_scroll, 0))
        .block(block)
}

/// Every finished job's output, newest at the bottom, scrolled up by
/// `log_scroll` lines.
fn log_pane(app: &App, height: u16) -> Paragraph<'static> {
    let mut lines: Vec<Line> = Vec::new();
    for entry in &app.log {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        let (verdict, color) = if entry.is_ok {
            ("ok", Color::Green)
        } else {
            ("failed", Color::Red)
        };
        lines.push(Line::styled(
            format!("{} tree {}: {verdict}", entry.kind.noun(), entry.tree),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
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
            Block::bordered()
                .title(" log ")
                .title_bottom(" PgUp/PgDn scroll   L closes ")
                .border_style(Style::new().fg(Color::White)),
        )
}
