//! Drawing the Issues screen: the list on top, the issue under the cursor
//! below with its description and comments, and the bar at the bottom.

use std::time::{SystemTime, UNIX_EPOCH};

use linear::Issue;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};

use crate::markdown;
use crate::picker::{Kind, Picker};
use crate::screen::{Focus, Screen};
use crate::{Palette, line};

/// The bar's keys, most used first; `?` lists them all.
const KEYS: [(&str, &str); 12] = [
    ("Enter", "pick up"),
    ("?", "help"),
    ("→", "read"),
    ("s", "status"),
    ("l", "labels"),
    ("a", "assign"),
    ("/", "filter"),
    ("m", "mine"),
    ("w", "source"),
    ("o", "open"),
    ("Tab", "trees"),
    ("q", "quit"),
];

/// Every key, for the `?` overlay.
const HELP: [(&str, &str); 17] = [
    ("Enter", "pick it up: an agent on it in a new tree"),
    ("→", "read it: the description and comments take the keys"),
    ("← h Esc", "back to the list"),
    ("s", "status: pick one, typing narrows the list"),
    ("l", "labels: Tab marks or unmarks each, Enter applies them"),
    ("a", "assignee: you, nobody, or a teammate"),
    (
        "/",
        "filter: mine label:Bug state:\"In Review\" project:x cycle:current, words",
    ),
    (
        "1 to 5",
        "views: mine, all, by project, by status, this cycle",
    ),
    ("6 to 9", "your own views, from [views] in the config"),
    ("m", "only yours, or everyone's"),
    ("w", "the next issue source: work, personal"),
    ("o", "open it in the browser"),
    ("r", "read the list and the team again"),
    ("j k ↑ ↓", "move, or scroll what you are reading"),
    ("g G", "top, bottom"),
    ("Tab", "back to the trees"),
    ("q", "quit"),
];

fn fg(color: ratatui::style::Color) -> Style {
    Style::new().fg(color)
}

/// How long ago `iso`, an RFC 3339 time, was, as short as it can be said:
/// `now`, `4m`, `3h`, `2d`, `5w`.
pub(crate) fn age(iso: &str) -> String {
    let Some(at) = seconds(iso) else {
        return String::new();
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let secs = now.saturating_sub(at);
    match secs {
        0..60 => "now".into(),
        60..3_600 => format!("{}m", secs / 60),
        3_600..86_400 => format!("{}h", secs / 3_600),
        86_400..1_209_600 => format!("{}d", secs / 86_400),
        _ => format!("{}w", secs / 604_800),
    }
}

/// Seconds since the epoch of `iso`, `2026-10-17T10:00:00.000Z`, read in UTC.
fn seconds(iso: &str) -> Option<u64> {
    let number = |range: std::ops::Range<usize>| iso.get(range)?.parse::<i64>().ok();
    let (y, m, d) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (h, min, s) = (number(11..13)?, number(14..16)?, number(17..19)?);
    // Days from the civil date, after Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3_600 + min * 60 + s).ok()
}

fn priority(issue: &Issue, p: &Palette) -> Cell<'static> {
    let (text, style) = match issue.priority {
        1 => ("urgent", fg(p.danger).add_modifier(Modifier::BOLD)),
        2 => ("high", fg(p.warning)),
        3 => ("medium", fg(p.text)),
        4 => ("low", fg(p.muted)),
        _ => ("", fg(p.muted)),
    };
    Cell::from(text).style(style)
}

fn key_bar(keys: &[(&str, &str)], width: u16, p: &Palette) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut used = 0;
    for (key, label) in keys {
        let pair = [
            Span::styled(format!(" {key} "), p.badge),
            Span::raw(format!(" {label} ")),
        ];
        let pair_width: usize = pair.iter().map(Span::width).sum();
        if used + pair_width > usize::from(width) {
            break;
        }
        used += pair_width;
        spans.extend(pair);
    }
    spans
}

impl Screen {
    pub fn draw(&self, frame: &mut Frame, area: Rect, p: &Palette) {
        let [list, detail, footer] = Layout::vertical([
            Constraint::Percentage(45),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(area);
        self.draw_list(frame, list, p);
        self.draw_detail(frame, detail, p);
        self.draw_footer(frame, footer, p);
        if self.is_help {
            draw_help(frame, area, p);
        } else if let Some(picker) = &self.picker {
            draw_picker(frame, area, picker, p);
        }
    }

    fn draw_list(&self, frame: &mut Frame, area: Rect, p: &Palette) {
        let note = match (&self.error, self.issues.is_empty(), self.is_loading) {
            (Some(err), true, _) => Some((format!(" {err}"), fg(p.danger))),
            (None, true, true) => Some((" reading issues...".to_string(), fg(p.muted))),
            (None, true, false) => Some((
                " no issues match; / changes the filter, m shows everyone's".to_string(),
                fg(p.muted),
            )),
            _ => None,
        };
        if let Some((text, style)) = note {
            frame.render_widget(
                Paragraph::new(text).style(style).wrap(Wrap { trim: false }),
                area,
            );
            return;
        }
        let header =
            Row::new(["ID", "STATUS", "PRIORITY", "TITLE", "LABELS", "ASSIGNEE"]).style(p.header);
        // A grouped list puts a heading row above each group, so the cursor's
        // row on screen counts the headings above it.
        let mut rows: Vec<Row> = Vec::new();
        let mut cursor_row = 0;
        for (at, issue) in self.issues.iter().enumerate() {
            if let Some(heading) = self.heading(at) {
                let count = self.issues[at..]
                    .iter()
                    .enumerate()
                    .take_while(|(n, _)| *n == 0 || self.heading(at + n).is_none())
                    .count();
                rows.push(Row::new(vec![
                    Cell::from("▾").style(fg(p.accent)),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(Line::from(vec![
                        Span::styled(heading, fg(p.title).add_modifier(Modifier::BOLD)),
                        Span::styled(format!("  {count}"), fg(p.muted)),
                    ])),
                ]));
            }
            if at == self.selected {
                cursor_row = rows.len();
            }
            rows.push(Row::new(vec![
                Cell::from(issue.id.clone()).style(fg(p.accent)),
                Cell::from(issue.state.clone()).style(fg(p.info)),
                priority(issue, p),
                Cell::from(issue.title.clone()),
                Cell::from(issue.labels.join(", ")).style(fg(p.special)),
                Cell::from(issue.assignee.clone().unwrap_or_default()).style(fg(p.muted)),
            ]));
        }
        let widths = [
            Constraint::Length(10),
            Constraint::Length(14),
            Constraint::Length(8),
            Constraint::Fill(1),
            Constraint::Length(18),
            Constraint::Length(14),
        ];
        let cursor = if self.focus == Focus::List {
            p.cursor
        } else {
            p.cursor.add_modifier(Modifier::DIM)
        };
        let table = Table::new(rows, widths)
            .header(header)
            .row_highlight_style(cursor);
        let mut state = TableState::default().with_selected(Some(cursor_row));
        frame.render_stateful_widget(table, area, &mut state);
    }

    fn draw_detail(&self, frame: &mut Frame, area: Rect, p: &Palette) {
        let border = if self.focus == Focus::Detail {
            p.accent
        } else {
            p.border
        };
        let block = Block::bordered().border_style(fg(border));
        let Some(issue) = self.current() else {
            frame.render_widget(block, area);
            return;
        };
        let block = block.title(Span::styled(
            format!(" {} ", issue.id),
            fg(p.accent).add_modifier(Modifier::BOLD),
        ));
        let detail = self.detail();
        // The row is what the list read and what edits update; the detail
        // adds the text and the comments.
        let mut lines = vec![
            Line::styled(
                issue.title.clone(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            fields(issue, p),
            Line::default(),
        ];
        match detail {
            None => lines.push(Line::styled("reading...", fg(p.muted))),
            Some(detail) => {
                match detail
                    .issue
                    .description
                    .as_deref()
                    .filter(|d| !d.trim().is_empty())
                {
                    Some(text) => lines.extend(markdown::render(text, p)),
                    None => lines.push(Line::styled(
                        "no description",
                        fg(p.muted).add_modifier(Modifier::ITALIC),
                    )),
                }
                if !detail.comments.is_empty() {
                    lines.push(Line::default());
                    lines.push(Line::styled(
                        format!("── {} comment(s) ", detail.comments.len()),
                        fg(p.border),
                    ));
                }
                for comment in &detail.comments {
                    lines.push(Line::default());
                    lines.push(Line::from(vec![
                        Span::styled(
                            comment.author.clone(),
                            fg(p.title).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(format!("  {}", age(&comment.created_at)), fg(p.muted)),
                    ]));
                    lines.extend(markdown::render(&comment.body, p));
                }
            }
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false })
                .scroll((self.scroll, 0)),
            area,
        );
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect, p: &Palette) {
        let count = match self.issues.len() {
            1 => "1 issue".to_string(),
            n => format!("{n} issues"),
        };
        let mut right = vec![Span::raw(format!("  {count}"))];
        let source = if self.source_count() > 1 {
            format!(
                "  {} ({}/{})",
                self.source_name(),
                self.source + 1,
                self.source_count()
            )
        } else {
            format!("  {}", self.source_name())
        };
        right.push(Span::raw(source));
        // A view says its name; a filter line of your own says itself.
        let narrowed = match (self.view.is_some(), line(&self.filters)) {
            (true, _) => self.view_name().to_string(),
            (false, filter) if filter.is_empty() => "all open".to_string(),
            (false, filter) => filter,
        };
        right.push(Span::styled(format!("  {narrowed}"), fg(p.accent)));
        if self.is_loading {
            right.push(Span::styled("  loading...", fg(p.info)));
        }
        right.push(Span::raw(" "));
        right.push(Span::styled(" treetop · issues ", p.badge));
        let right = Line::from(right);
        let right_width = u16::try_from(right.width())
            .unwrap_or(u16::MAX)
            .min(area.width);
        let [left_area, right_area] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(right_width)]).areas(area);
        let left = if let Some(text) = &self.editing {
            Line::raw(format!(
                " Filter: {text}_   Enter applies it, Esc keeps the last"
            ))
        } else if let Some(message) = &self.message {
            Line::styled(format!(" {message}"), fg(p.warning))
        } else if let (Some(err), false) = (&self.error, self.issues.is_empty()) {
            Line::styled(format!(" {err}"), fg(p.danger))
        } else {
            Line::from(key_bar(&KEYS, left_area.width, p))
        };
        frame.render_widget(Paragraph::new(left).style(p.bar), left_area);
        frame.render_widget(Paragraph::new(right).style(p.bar), right_area);
    }
}

/// The issue's fields on one line: status, priority, labels, assignee,
/// project and cycle, each only when set.
fn fields(issue: &Issue, p: &Palette) -> Line<'static> {
    let mut spans = vec![Span::styled(issue.state.clone(), fg(p.info))];
    let mut add = |text: String, style: Style| {
        spans.push(Span::styled("  ·  ", fg(p.border)));
        spans.push(Span::styled(text, style));
    };
    if issue.priority > 0 {
        add(issue.priority_label().to_string(), fg(p.warning));
    }
    if !issue.labels.is_empty() {
        add(issue.labels.join(", "), fg(p.special));
    }
    add(
        issue
            .assignee
            .clone()
            .unwrap_or_else(|| "unassigned".into()),
        fg(p.muted),
    );
    if let Some(project) = &issue.project {
        add(project.clone(), fg(p.text));
    }
    if let Some(cycle) = &issue.cycle {
        add(cycle.clone(), fg(p.muted));
    }
    Line::from(spans)
}

/// The picker in the middle of the screen: what is picked for which issue,
/// the query, and the choices it keeps, the issue's own marked.
fn draw_picker(frame: &mut Frame, area: Rect, picker: &Picker, p: &Palette) {
    let shown = picker.shown();
    let width = picker
        .choices
        .iter()
        .map(|c| c.name.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(24, 60)
        + 10;
    let rows = shown.len().clamp(1, 16);
    let height = u16::try_from(rows + 4).unwrap_or(u16::MAX).min(area.height);
    let width = u16::try_from(width).unwrap_or(u16::MAX).min(area.width);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let hint = match picker.kind {
        Kind::Labels => " Tab marks  Enter applies  Esc cancels ",
        _ => " Enter picks  Esc cancels ",
    };
    let block = Block::bordered()
        .border_style(fg(p.accent))
        .title(Span::styled(
            format!(" {} of {} ", picker.kind.noun(), picker.issue),
            fg(p.title).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::styled(hint, fg(p.muted)));
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    let mut lines = vec![
        Line::from(vec![
            Span::styled("› ", fg(p.accent)),
            Span::raw(picker.query.clone()),
            Span::styled("_", fg(p.muted)),
        ]),
        Line::default(),
    ];
    // The cursor stays in view in a long list.
    let first = picker.cursor.saturating_sub(rows.saturating_sub(1));
    for (at, choice) in shown.iter().enumerate().skip(first).take(rows) {
        let is_marked = match picker.kind {
            Kind::Labels => choice
                .id
                .as_ref()
                .is_some_and(|id| picker.marked.contains(id)),
            _ => choice.id == picker.current,
        };
        let mark = match (picker.kind, is_marked) {
            (Kind::Labels, true) => "[x] ",
            (Kind::Labels, false) => "[ ] ",
            (_, true) => "● ",
            (_, false) => "  ",
        };
        let style = if at == picker.cursor {
            p.cursor
        } else {
            Style::new()
        };
        lines.push(
            Line::from(vec![
                Span::styled(mark, fg(p.accent)),
                Span::raw(choice.name.clone()),
            ])
            .style(style),
        );
    }
    if shown.is_empty() {
        lines.push(Line::styled("nothing matches", fg(p.muted)));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_help(frame: &mut Frame, area: Rect, p: &Palette) {
    let width = HELP
        .iter()
        .map(|(_, does)| does.chars().count())
        .max()
        .unwrap_or(0)
        + 14;
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(key, does)| {
            Line::from(vec![
                Span::styled(
                    format!(" {key:<9} "),
                    fg(p.accent).add_modifier(Modifier::BOLD),
                ),
                Span::raw(does.to_string()),
            ])
        })
        .collect();
    let height = u16::try_from(lines.len() + 2)
        .unwrap_or(u16::MAX)
        .min(area.height);
    let width = u16::try_from(width).unwrap_or(u16::MAX).min(area.width);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_style(fg(p.accent))
                .title(Span::styled(
                    " Keys ",
                    fg(p.title).add_modifier(Modifier::BOLD),
                ))
                .title_bottom(Line::styled(" any key closes ", fg(p.muted))),
        ),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_rfc_3339_time_in_utc() {
        assert_eq!(seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(seconds("2026-10-17T10:00:00.000Z"), Some(1_792_231_200));
        assert_eq!(seconds("garbage"), None);
        assert_eq!(age("garbage"), "");
    }
}
