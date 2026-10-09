//! Drawing, htop style: a summary line, the pool table, a detail pane for the
//! tree under the cursor, and a key bar. Every colour comes from `theme`.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};

use crate::app::{App, DiffView, JobState, Pane, Pending, Review};
use crate::diff::{Change, FileDiff, Layout as DiffLayout, Row as DiffRow, Side, ViewRow};
use crate::pool::{Tree, Work};
use crate::theme::theme;

/// Most useful first: on a narrow bar the keys at the end are the ones left
/// out.
const KEYS: [(&str, &str); 11] = [
    ("Enter", "open"),
    ("Tab", "diff"),
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

pub fn draw(frame: &mut Frame, app: &App) {
    // The theme's text colour is the base for every cell; widgets below only
    // add accents, so nothing falls back to a theme's dim default.
    frame.render_widget(Block::new().style(fg(theme().text)), frame.area());
    if let Some(view) = &app.view {
        draw_diff_view(frame, app, view);
        return;
    }
    let [body, detail, footer] = Layout::vertical([
        Constraint::Fill(1),
        if app.pane == Pane::Info {
            Constraint::Length(4)
        } else {
            Constraint::Fill(1)
        },
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_table(frame, app, body);
    match app.pane {
        Pane::Info => frame.render_widget(detail_pane(app), detail),
        Pane::Log => frame.render_widget(log_pane(app, detail.height), detail),
    }
    draw_footer(frame, app, footer);
    if let Some(review) = app.reviews.front() {
        draw_review(frame, review, app.reviews.len() - 1);
    } else if let Some(pending) = &app.pending {
        draw_confirm(frame, pending);
    }
}

/// A bar across the screen: the name as a badge, then `spans`.
fn bar(name: &str, spans: Vec<Span<'static>>) -> Paragraph<'static> {
    let t = theme();
    let mut line = vec![Span::styled(format!(" {name} "), t.badge), Span::raw(" ")];
    line.extend(spans);
    Paragraph::new(Line::from(line)).style(t.bar)
}

/// The pool at a glance, for the right end of the bottom bar: counts,
/// actions and how fresh the listing is, closed by the name as a badge, the
/// way lualine closes its statusline. Zero marks and actions are left out.
fn summary(app: &App) -> Vec<Span<'static>> {
    let t = theme();
    let mut spans = vec![Span::raw("  ")];
    if app.is_loaded {
        let held = app.trees.iter().filter(|t| t.is_held()).count();
        let running = app.trees.iter().filter(|t| !t.processes.is_empty()).count();
        spans.push(Span::raw(format!(
            "{} trees  {held} held  ",
            app.trees.len()
        )));
        spans.push(Span::styled(format!("{running} running"), fg(t.warning)));
        if !app.marked.is_empty() {
            spans.push(Span::raw(format!("  {} marked", app.marked.len())));
        }
        if let n @ 1.. = app.active_jobs() {
            spans.push(Span::styled(
                format!("  {n} action(s) running"),
                fg(t.info).add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(freshness(app));
        if app.is_listing {
            spans.push(Span::styled("  refreshing...", fg(t.info)));
        }
    } else {
        spans.push(Span::raw("reading the pool..."));
    }
    spans.push(Span::raw(" "));
    spans.push(Span::styled(" treetop ", t.badge));
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
                _ => t.danger,
            };
            (tree.status.clone(), fg(color))
        }
    };
    Cell::from(text).style(style)
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
        Cell::from(tree.holder.clone().unwrap_or_default()),
        count_cell(procs, t.warning),
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
        "", "#", "STATUS", "BRANCH", "HOLDER", "PROCS", "CHANGED", "UNPUSHED",
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
        Constraint::Length(20),
        Constraint::Length(6),
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
        Line::from(vec![
            Span::styled("processes ", fg(t.muted)),
            Span::raw(procs),
        ]),
    ])
    .block(pane_block())
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
    let mut spans = fitting_keys(&KEYS, width);
    if !app.filter.is_empty() {
        spans.push(Span::styled(
            format!(" filter: {}", app.filter),
            fg(t.accent),
        ));
    }
    Line::from(spans)
}

/// The bottom bar: keys or a message on the left, the summary on the right.
fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let right = Line::from(summary(app));
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
    let area = centered(frame.area(), 80, height);
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

/// The full-screen side-by-side diff of one tree against its base.
fn draw_diff_view(frame: &mut Frame, app: &App, view: &DiffView) {
    let t = theme();
    let [top, body, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let tree = app.trees.iter().find(|tree| tree.path == view.path);
    let mut title = vec![Span::raw(format!(
        "tree {}  {}",
        tree.map_or("?", |tree| tree.name.as_str()),
        tree.and_then(|tree| tree.branch.as_deref())
            .unwrap_or("(detached)")
    ))];
    match app.diffs.get(&view.path).map(|d| &d.layout) {
        None => frame.render_widget(Paragraph::new(" reading the diff..."), body),
        Some(Err(err)) => {
            frame.render_widget(Paragraph::new(format!(" {err}")).style(fg(t.danger)), body)
        }
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
            title.push(Span::styled(format!("+{adds}"), fg(t.success)));
            title.push(Span::raw(" "));
            title.push(Span::styled(format!("-{dels}"), fg(t.danger)));
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
                        .style(fg(t.success)),
                    body,
                );
            } else {
                draw_diff_rows(frame, layout, view, body);
            }
        }
    }
    frame.render_widget(bar("diff", title), top);
    frame.render_widget(
        Paragraph::new(Line::from(fitting_keys(&DIFF_KEYS, keys.width))).style(t.bar),
        keys,
    );
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
