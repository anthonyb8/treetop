//! Drawing, htop style: a summary line, the pool table, a detail pane for the
//! tree under the cursor, and a key bar.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState};

use crate::app::{Action, App, Pending};
use crate::pool::{Tree, Work};

const KEYS: [(&str, &str); 8] = [
    ("Space", "mark"),
    ("u", "unmark"),
    ("/", "filter"),
    ("Enter", "cd"),
    ("r", "return"),
    ("D", "destroy"),
    ("j/k", "move"),
    ("q", "quit"),
];

pub fn draw(frame: &mut Frame, app: &App) {
    let [summary, body, detail, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(4),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(summary_line(app), summary);
    draw_table(frame, app, body);
    frame.render_widget(detail_pane(app), detail);
    frame.render_widget(footer_line(app), footer);
    if let Some(pending) = &app.pending {
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
            Span::styled("  reading the pool...", Style::new().fg(Color::DarkGray)),
        ]));
    }
    let held = app.trees.iter().filter(|t| t.is_held()).count();
    let running = app.trees.iter().filter(|t| !t.processes.is_empty()).count();
    Paragraph::new(Line::from(vec![
        Span::styled("treetop", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(format!("  {} trees  {held} held  ", app.trees.len())),
        Span::styled(format!("{running} running"), Style::new().fg(Color::Yellow)),
        Span::raw(format!("  {} marked", app.marked.len())),
    ]))
}

fn count_cell(value: Option<usize>, color: Color) -> Cell<'static> {
    match value {
        Some(0) => Cell::from("0").style(Style::new().fg(Color::DarkGray)),
        Some(n) => Cell::from(n.to_string()).style(Style::new().fg(color)),
        None => Cell::from("-").style(Style::new().fg(Color::DarkGray)),
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
    let status_color = match tree.status.as_str() {
        "leased" => Color::Green,
        "available" => Color::DarkGray,
        _ => Color::Red,
    };
    let branch = match &tree.branch {
        Some(b) => Cell::from(b.clone()),
        None => Cell::from("(detached)").style(Style::new().fg(Color::DarkGray)),
    };
    let procs = (!tree.processes.is_empty() || tree.is_held()).then_some(tree.processes.len());
    Row::new(vec![
        Cell::from(mark),
        Cell::from(tree.name.clone()),
        Cell::from(tree.status.clone()).style(Style::new().fg(status_color)),
        branch,
        Cell::from(tree.holder.clone().unwrap_or_default()),
        count_cell(procs, Color::Yellow),
        work_cell(tree, |w| w.changed, Color::Red),
        work_cell(tree, |w| w.unpushed, Color::Magenta),
    ])
}

fn draw_table(frame: &mut Frame, app: &App, area: Rect) {
    let header = Row::new([
        "", "#", "STATUS", "BRANCH", "HOLDER", "PROCS", "CHANGED", "UNPUSHED",
    ])
    .style(Style::new().fg(Color::Black).bg(Color::Green));
    let rows: Vec<Row> = app.visible().into_iter().map(|t| row(app, t)).collect();
    let widths = [
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Length(10),
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
    let block = Block::bordered().border_style(Style::new().fg(Color::DarkGray));
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
                Style::new().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled("processes ", Style::new().fg(Color::DarkGray)),
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
    let key = Style::new().fg(Color::Black).bg(Color::Cyan);
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
            match t.work {
                Some(w) if w.changed + w.unpushed > 0 => spans.push(Span::styled(
                    format!("  {} changed, {} unpushed", w.changed, w.unpushed),
                    Style::new().fg(Color::Red),
                )),
                None if t.is_held() => spans.push(Span::styled(
                    "  git cannot read it",
                    Style::new().fg(Color::Yellow),
                )),
                _ => {}
            }
            Line::from(spans)
        })
        .collect();
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        match pending.action {
            Action::Return => " y returns them to the pool; any other key cancels",
            Action::Destroy => " y shows treehouse's preview for each, then asks again",
        },
        Style::new().fg(Color::DarkGray),
    ));
    let title = format!(
        " {} {} tree(s)? ",
        pending.action.verb(),
        pending.trees.len()
    );
    let color = match pending.action {
        Action::Return => Color::Yellow,
        Action::Destroy => Color::Red,
    };
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = centered(frame.area(), 70, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(title)
                .border_style(Style::new().fg(color)),
        ),
        area,
    );
}
