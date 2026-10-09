//! Drawing, htop style: a summary line, the pool table, a detail pane for the
//! tree under the cursor, and a key bar.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};

use crate::app::{App, DiffView, JobState, Pane, Pending, Review};
use crate::diff::{Change, FileDiff, Layout as DiffLayout, Row as DiffRow, Side, ViewRow};
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
    // White is the base for every cell; widgets below only add accents, so
    // nothing falls back to a terminal theme's default or dim foreground.
    frame.render_widget(
        Block::new().style(Style::new().fg(Color::White)),
        frame.area(),
    );
    if let Some(view) = &app.view {
        draw_diff_view(frame, app, view);
        return;
    }
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

    frame.render_widget(summary_line(app), summary);
    draw_table(frame, app, body);
    match app.pane {
        Pane::Info => frame.render_widget(detail_pane(app), detail),
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

/// Removed lines on the left, added on the right, from the terminal's own 16
/// ANSI colours, as Claude Code's dark-ansi theme draws diffs, so they follow
/// whatever palette the terminal runs (gruvbox, say) instead of fixed shades.
const REMOVED: Color = Color::Red;
const ADDED: Color = Color::Green;
/// The chars within a changed line that actually differ from its pair.
const REMOVED_STRONG: Color = Color::LightRed;
const ADDED_STRONG: Color = Color::LightGreen;
/// Text on those backgrounds: the palette's black, which reads on red and
/// green in dark palettes where white would not.
const ON_CHANGE: Color = Color::Black;

const DIFF_KEYS: [(&str, &str); 8] = [
    ("j/k", "line"),
    ("Space/b", "page"),
    ("d/u", "half page"),
    ("n/p", "file"),
    ("h/l", "sideways"),
    ("g/G", "top/end"),
    ("^R", "reload"),
    ("Esc", "back"),
];

fn key_bar(keys: &[(&str, &str)]) -> Line<'static> {
    let key = Style::new()
        .fg(Color::White)
        .bg(Color::Blue)
        .add_modifier(Modifier::BOLD);
    Line::from(
        keys.iter()
            .flat_map(|(k, label)| {
                [
                    Span::styled(format!(" {k} "), key),
                    Span::raw(format!(" {label}  ")),
                ]
            })
            .collect::<Vec<_>>(),
    )
}

/// One half of a split row: the line number, then the text scrolled sideways
/// by `hscroll` columns, with the chars that differ from its pair on `strong`.
fn side_line(
    side: Option<&Side>,
    (change, strong): (Color, Color),
    number_width: usize,
    hscroll: u16,
) -> Paragraph<'static> {
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
    let number = if side.is_change {
        Style::new().fg(ON_CHANGE)
    } else {
        Style::new().fg(Color::Yellow)
    };
    let mut spans = vec![Span::styled(
        format!("{:>number_width$} ", side.number),
        number,
    )];
    spans.push(Span::raw(piece(0..from)));
    spans.push(Span::styled(
        piece(from..to),
        Style::new().bg(strong).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw(piece(to..chars.len())));
    let style = if side.is_change {
        Style::new().bg(change).fg(ON_CHANGE)
    } else {
        Style::new()
    };
    Paragraph::new(Line::from(spans)).style(style)
}

fn change_letter(change: Change) -> (&'static str, Color) {
    match change {
        Change::Added => ("A", Color::LightGreen),
        Change::Deleted => ("D", Color::LightRed),
        Change::Modified => ("M", Color::LightYellow),
        Change::Renamed => ("R", Color::LightCyan),
    }
}

/// A file's header bar: its change, path and line counts.
fn file_header(file: &FileDiff) -> Paragraph<'static> {
    let (letter, color) = change_letter(file.change);
    let path = match &file.from {
        Some(from) => format!("{from} -> {}", file.path),
        None => file.path.clone(),
    };
    Paragraph::new(Line::from(vec![
        Span::styled(format!(" {letter} "), Style::new().fg(color)),
        Span::raw(format!(" {path}  ")),
        Span::styled(
            format!("+{}", file.additions),
            Style::new().fg(Color::LightGreen),
        ),
        Span::raw(" "),
        Span::styled(
            format!("-{}", file.deletions),
            Style::new().fg(Color::LightRed),
        ),
    ]))
    .style(
        Style::new()
            .fg(Color::White)
            .bg(Color::Blue)
            .add_modifier(Modifier::BOLD),
    )
}

/// The visible slice of the one scroll through every file. Only these rows
/// are built each frame, so a diff of fifty thousand lines scrolls as fast
/// as one of fifty. While the top row is inside a file, that file's header
/// stays pinned above it, so the path is always on screen.
fn draw_diff_rows(frame: &mut Frame, layout: &DiffLayout, view: &DiffView, body: Rect) {
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
                Paragraph::new("    binary file: contents not shown")
                    .style(Style::new().fg(Color::Yellow)),
                full,
            ),
            ViewRow::Line(f, r) => match &diff.files[*f].rows[*r] {
                DiffRow::Hunk(header) => frame.render_widget(
                    Paragraph::new(header.clone()).style(Style::new().fg(Color::Cyan)),
                    full,
                ),
                DiffRow::Pair(left, right) => {
                    let left_area = Rect::new(body.x, y, half, 1);
                    let right_area =
                        Rect::new(body.x + half + 1, y, body.width.saturating_sub(half + 1), 1);
                    frame.render_widget(
                        side_line(
                            left.as_ref(),
                            (REMOVED, REMOVED_STRONG),
                            number_width,
                            view.hscroll,
                        ),
                        left_area,
                    );
                    frame.render_widget(Paragraph::new("│"), Rect::new(body.x + half, y, 1, 1));
                    frame.render_widget(
                        side_line(
                            right.as_ref(),
                            (ADDED, ADDED_STRONG),
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

/// The full-screen side-by-side diff of one tree against its base.
fn draw_diff_view(frame: &mut Frame, app: &App, view: &DiffView) {
    let [top, body, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let tree = app.trees.iter().find(|t| t.path == view.path);
    let mut title = vec![
        Span::styled(" diff ", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(format!(
            " tree {}  {}",
            tree.map_or("?", |t| t.name.as_str()),
            tree.and_then(|t| t.branch.as_deref())
                .unwrap_or("(detached)")
        )),
    ];
    match app.diffs.get(&view.path).map(|d| &d.layout) {
        None => frame.render_widget(Paragraph::new(" reading the diff..."), body),
        Some(Err(err)) => frame.render_widget(
            Paragraph::new(format!(" {err}")).style(Style::new().fg(Color::Red)),
            body,
        ),
        Some(Ok(layout)) => {
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
            title.push(Span::styled(
                format!("+{adds}"),
                Style::new().fg(Color::Green),
            ));
            title.push(Span::raw(" "));
            title.push(Span::styled(
                format!("-{dels}"),
                Style::new().fg(Color::Red),
            ));
            if let Some(index) = layout.file_starts.iter().rposition(|&s| s <= view.scroll) {
                title.push(Span::raw(format!(
                    "   file {}/{}: {}",
                    index + 1,
                    diff.files.len(),
                    diff.files[index].path
                )));
            }
            if layout.rows.is_empty() {
                frame.render_widget(
                    Paragraph::new(format!(" No changes against {}.", diff.base))
                        .style(Style::new().fg(Color::Green)),
                    body,
                );
            } else {
                draw_diff_rows(frame, layout, view, body);
            }
        }
    }
    frame.render_widget(Paragraph::new(Line::from(title)), top);
    frame.render_widget(Paragraph::new(key_bar(&DIFF_KEYS)), keys);
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
