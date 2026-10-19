//! Drawing the Issues screen: the list on top, the issue under the cursor
//! below with its description and comments, and the bar at the bottom. An
//! open issue takes the whole screen.

use std::time::{SystemTime, UNIX_EPOCH};

use linear::{Issue, Link};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Cell, Clear, Padding, Paragraph, Row, StatefulWidget, Table, TableState, Wrap,
};
use ratatui_image::{Resize, StatefulImage};

use crate::Palette;
use crate::compose::Compose;
use crate::form::{FIELDS, Field, Form};
use crate::host::Host;
use crate::markdown::{self, Piece};
use crate::panel::{self, ROWS};
use crate::picker::{Kind, Picker};
use crate::picture::{self, Picture};
use crate::screen::{Focus, Screen};
use crate::targets::Target;

/// The bar's keys, most used first; `?` lists them all.
const KEYS: [(&str, &str); 17] = [
    ("Enter", "read"),
    ("p", "pick up"),
    ("?", "help"),
    ("z", "fold"),
    ("s", "status"),
    ("l", "labels"),
    ("a", "assign"),
    ("c", "comment"),
    ("e", "edit"),
    ("n", "new"),
    ("/", "search"),
    ("f", "filters"),
    ("m", "mine"),
    ("w", "source"),
    ("o", "open"),
    ("Tab", "trees"),
    ("q", "quit"),
];

/// The bar's keys inside an open issue.
const ISSUE_KEYS: [(&str, &str); 13] = [
    ("Esc", "back"),
    ("Tab", "links, images"),
    ("Enter", "open it"),
    ("j k", "scroll"),
    ("p", "pick up"),
    ("e", "edit"),
    ("s", "status"),
    ("l", "labels"),
    ("a", "assign"),
    ("c", "comment"),
    ("o", "open"),
    ("?", "help"),
    ("q", "quit"),
];

/// The bar's keys over an image on the whole screen.
const VIEWER_KEYS: [(&str, &str); 3] = [
    ("Esc", "back"),
    ("← →", "other images"),
    ("o", "open it in the browser"),
];

/// The bar's keys in the form, by the field the cursor is in.
fn form_keys(field: Field) -> &'static [(&'static str, &'static str)] {
    match field {
        Field::Title => &[("Tab", "next field"), ("Ctrl+S", "save"), ("Esc", "cancel")],
        Field::Description => &[
            ("Enter", "new line"),
            ("Ctrl+E", "$EDITOR"),
            ("Shift+Tab", "back"),
            ("Ctrl+S", "save"),
            ("Esc", "cancel"),
        ],
        _ => &[
            ("Enter", "change"),
            ("Tab", "next field"),
            ("Ctrl+S", "save"),
            ("Esc", "cancel"),
        ],
    }
}

/// Every key, for the `?` overlay.
const HELP: [(&str, &str); 25] = [
    (
        "Enter →",
        "read it: the whole issue, its description and comments",
    ),
    ("← h Esc", "back to the list"),
    (
        "J K PgDn PgUp",
        "scroll the preview below the list, a line or a page",
    ),
    ("p", "pick it up: an agent on it in a new tree"),
    ("s", "status: pick one, typing narrows the list"),
    ("l", "labels: Tab marks or unmarks each, Enter applies them"),
    ("a", "assignee: you, nobody, or a teammate"),
    (
        "c",
        "comment: Markdown, Alt+Enter for a new line, Enter sends",
    ),
    (
        "e",
        "edit: title, fields and description in one form, Ctrl+S saves",
    ),
    ("n", "new issue: the same form, every field in one go"),
    (
        "/",
        "search: narrows the list as you type, ids and titles; Esc clears it",
    ),
    (
        "f",
        "filters: assignee, status, label, project, cycle, grouping",
    ),
    (
        "z Z",
        "fold the status or project group under the cursor, or every group",
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
    (
        "Tab Enter",
        "in an issue: move to a link or image, and open it; Esc back",
    ),
    ("j k ↑ ↓", "move, or scroll the issue you are reading"),
    ("Space PgUp", "a page down or up in the issue"),
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

/// Which tree is working on the issue, and its agent's state: `4 ·
/// waiting` loud, a busy one green, the rest muted.
fn work_cell(host: &dyn Host, id: &str, p: &Palette) -> Cell<'static> {
    let Some(work) = host.work_on(id) else {
        return Cell::from("");
    };
    if work.is_ready {
        return Cell::from("ready").style(fg(p.success).add_modifier(Modifier::BOLD));
    }
    let agent = work.agent.as_deref().unwrap_or("no agent");
    let style = if work.needs_you {
        fg(p.warning).add_modifier(Modifier::BOLD)
    } else if agent == "busy" {
        fg(p.success)
    } else {
        fg(p.muted)
    };
    Cell::from(format!("{} · {agent}", work.tree)).style(style)
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
    /// Draws the screen in `area`, the WORK column from what `host` knows.
    pub fn draw(&self, frame: &mut Frame, area: Rect, p: &Palette, host: &dyn Host) {
        if let Some(form) = &self.form {
            let [body, footer] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
            let team = self
                .teams
                .get(&self.source)
                .map_or(String::new(), |t| t.key.clone());
            draw_form(frame, body, form, &team, p, self.picker.is_none());
            self.draw_footer(frame, footer, p);
        } else if self.focus == Focus::Issue {
            let [issue, footer] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
            match self.viewer {
                Some(at) => self.draw_viewer(frame, issue, at, p),
                None => self.draw_reading(frame, issue, p),
            }
            self.draw_footer(frame, footer, p);
        } else {
            let [list, detail, footer] = Layout::vertical([
                Constraint::Percentage(50),
                Constraint::Fill(1),
                Constraint::Length(1),
            ])
            .areas(area);
            self.draw_list(frame, list, p, host);
            self.draw_detail(frame, detail, p);
            self.draw_footer(frame, footer, p);
        }
        if self.is_help {
            draw_help(frame, area, p);
        } else if let (Some(at), None) = (self.panel, &self.picker) {
            self.draw_panel(frame, area, at, p);
        } else if let Some(picker) = &self.picker {
            draw_picker(frame, area, picker, p);
        } else if let Some(compose) = &self.compose {
            draw_compose(frame, area, compose, p);
        }
    }

    fn draw_list(&self, frame: &mut Frame, area: Rect, p: &Palette, host: &dyn Host) {
        let [bar, area] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        self.draw_bar(frame, bar, p);
        let note = match (&self.error, self.issues.is_empty(), self.is_loading) {
            (Some(err), true, _) => Some((format!(" {err}"), fg(p.danger))),
            (None, true, true) => Some((" reading issues...".to_string(), fg(p.muted))),
            (None, true, false) => Some((
                " no issues match; f changes the filters, / the search".to_string(),
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
        let header = Row::new([
            "ID", "STATUS", "PRIORITY", "TITLE", "LABELS", "ASSIGNEE", "WORK",
        ])
        .style(p.header);
        // A grouped list puts a heading row above each group, so the cursor's
        // row on screen counts the headings above it.
        let mut rows: Vec<Row> = Vec::new();
        let mut cursor_row = 0;
        for (at, issue) in self.issues.iter().enumerate() {
            let is_folded = self.is_folded(at);
            if let Some(heading) = self.heading(at) {
                let count = self.issues[at..]
                    .iter()
                    .enumerate()
                    .take_while(|(n, _)| *n == 0 || self.heading(at + n).is_none())
                    .count();
                // A folded group's heading is its only row, so the cursor
                // anywhere in it sits on the heading.
                if is_folded && self.group_of(self.selected).as_ref() == Some(&heading) {
                    cursor_row = rows.len();
                }
                let glyph = if is_folded { "▸" } else { "▾" };
                rows.push(Row::new(vec![
                    Cell::from(glyph).style(fg(p.accent)),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(Line::from(vec![
                        Span::styled(heading, fg(p.title).add_modifier(Modifier::BOLD)),
                        Span::styled(format!("  {count}"), fg(p.muted)),
                    ])),
                ]));
            }
            if is_folded {
                continue;
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
                work_cell(host, &issue.id, p),
            ]));
        }
        let widths = [
            Constraint::Length(10),
            Constraint::Length(14),
            Constraint::Length(8),
            Constraint::Fill(1),
            Constraint::Length(18),
            Constraint::Length(14),
            Constraint::Length(14),
        ];
        let table = Table::new(rows, widths)
            .header(header)
            .row_highlight_style(p.cursor);
        let mut state = TableState::default().with_selected(Some(cursor_row));
        frame.render_stateful_widget(table, area, &mut state);
    }

    /// The line above the list, saying what it shows: whose issues, which,
    /// whatever else narrows them, and the search, or the search box while
    /// it is typed in.
    fn draw_bar(&self, frame: &mut Frame, area: Rect, p: &Palette) {
        if let Some(text) = &self.search {
            let line = Line::from(vec![
                Span::styled(" Search › ", fg(p.accent).add_modifier(Modifier::BOLD)),
                Span::raw(text.clone()),
            ]);
            let x = u16::try_from(line.width())
                .unwrap_or(0)
                .min(area.width.saturating_sub(1));
            frame.render_widget(Paragraph::new(line), area);
            frame.set_cursor_position((area.x + x, area.y));
            return;
        }
        if let Some(what) = &self.choosing {
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(" Choose ", fg(p.accent).add_modifier(Modifier::BOLD)),
                    Span::styled(what.clone(), fg(p.title).add_modifier(Modifier::BOLD)),
                    Span::styled("  ·  Enter chooses it, Esc cancels", fg(p.muted)),
                ])),
                area,
            );
            return;
        }
        if self.is_without_source {
            frame.render_widget(
                Paragraph::new(" no issue source for this repository").style(fg(p.muted)),
                area,
            );
            return;
        }
        let mut spans = vec![Span::raw(" ")];
        for (at, piece) in panel::describe(&self.filters, self.group)
            .into_iter()
            .enumerate()
        {
            if at > 0 {
                spans.push(Span::styled("  ·  ", fg(p.border)));
            }
            let style = if at == 0 {
                fg(p.title).add_modifier(Modifier::BOLD)
            } else {
                fg(p.info)
            };
            spans.push(Span::styled(piece, style));
        }
        if let Some(search) = self.search_text() {
            spans.push(Span::styled("  ·  ", fg(p.border)));
            spans.push(Span::styled(format!("“{search}”"), fg(p.special)));
        }
        let hint = Line::from(vec![
            Span::styled("f", fg(p.accent)),
            Span::styled(" filters  ", fg(p.muted)),
            Span::styled("/", fg(p.accent)),
            Span::styled(" search ", fg(p.muted)),
        ]);
        let hint_width = u16::try_from(hint.width()).unwrap_or(0);
        let [left, right] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(hint_width)]).areas(area);
        frame.render_widget(Paragraph::new(Line::from(spans)), left);
        frame.render_widget(Paragraph::new(hint), right);
    }

    /// The filter panel over the list: a row per filter, its value, the
    /// cursor's row marked.
    fn draw_panel(&self, frame: &mut Frame, area: Rect, at: usize, p: &Palette) {
        let rows: Vec<Line> = ROWS
            .iter()
            .enumerate()
            .map(|(n, row)| {
                let value = panel::value(*row, &self.filters, self.group);
                let is_set = panel::current(*row, &self.filters, self.group).is_some();
                let mark = if n == at { "› " } else { "  " };
                let line = Line::from(vec![
                    Span::styled(format!("{mark}{:<10}", row.label()), fg(p.muted)),
                    Span::styled(
                        value,
                        if is_set {
                            fg(p.info).add_modifier(Modifier::BOLD)
                        } else {
                            fg(p.text)
                        },
                    ),
                ]);
                if n == at { line.style(p.cursor) } else { line }
            })
            .collect();
        let width = 52.min(area.width);
        let height = u16::try_from(rows.len() + 2).unwrap_or(8).min(area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 3,
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(rows).block(Block::bordered().border_style(fg(p.accent)).title(
                Span::styled(" Filters ", fg(p.title).add_modifier(Modifier::BOLD)),
            )),
            popup,
        );
    }

    /// The issue under the cursor, below the list: as much as fits.
    fn draw_detail(&self, frame: &mut Frame, area: Rect, p: &Palette) {
        let block = Block::bordered()
            .border_style(fg(p.border))
            .padding(Padding::horizontal(1));
        let Some(issue) = self.current() else {
            let folded = self
                .group_of(self.selected)
                .filter(|_| self.is_folded(self.selected));
            match folded {
                Some(group) => {
                    let count = (0..self.issues.len())
                        .filter(|at| self.group_of(*at).as_ref() == Some(&group))
                        .count();
                    let text = format!("{group}: {count} issue(s), folded; z or Enter opens it");
                    frame.render_widget(
                        Paragraph::new(Line::styled(text, fg(p.muted))).block(block),
                        area,
                    );
                }
                None => frame.render_widget(block, area),
            }
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
        let inner = block.inner(area);
        let text = Paragraph::new(lines).wrap(Wrap { trim: false });
        let height = u16::try_from(text.line_count(inner.width)).unwrap_or(u16::MAX);
        let max = height.saturating_sub(inner.height);
        self.preview_max.set(max);
        self.preview_height.set(inner.height);
        self.preview_area.set(area);
        let scroll = self.preview_scroll(&issue.id);
        let block = if max > 0 {
            block.title_bottom(Line::styled(
                format!(
                    " {}%  J K or PgDn PgUp scroll it, Enter reads it whole ",
                    u32::from(scroll) * 100 / u32::from(max)
                ),
                fg(p.muted),
            ))
        } else {
            block
        };
        frame.render_widget(block, area);
        frame.render_widget(text.scroll((scroll, 0)), inner);
    }

    /// The open issue over the whole of `area`: its title and fields, its
    /// links, its description with its images, and its comments, scrolled.
    fn draw_reading(&self, frame: &mut Frame, area: Rect, p: &Palette) {
        // Where it was opened from, then each issue opened from a link; one
        // opened from the trees starts the trail itself.
        let mut trail: Vec<&str> = self
            .issues
            .get(self.selected)
            .filter(|_| !self.is_from_outside)
            .map(|i| i.id.as_str())
            .into_iter()
            .collect();
        trail.extend(self.trail.iter().map(String::as_str));
        let mut title = vec![Span::raw(" ")];
        for (at, id) in trail.iter().enumerate() {
            if at + 1 == trail.len() {
                title.push(Span::styled(*id, fg(p.accent).add_modifier(Modifier::BOLD)));
            } else {
                title.push(Span::styled(format!("{id} › "), fg(p.muted)));
            }
        }
        title.push(Span::raw(" "));
        let block = Block::bordered()
            .border_style(fg(p.accent))
            .padding(Padding::horizontal(1))
            .title(Line::from(title));
        let inner = block.inner(area);
        self.hits.borrow_mut().clear();
        self.placed.borrow_mut().clear();
        let Some(issue) = self.current() else {
            frame.render_widget(block, area);
            frame.render_widget(
                Paragraph::new(format!(
                    "reading {}...",
                    self.viewed_id().unwrap_or_default()
                ))
                .style(fg(p.muted)),
                inner,
            );
            return;
        };
        let parts = self.reading_parts(issue, p);
        let heights: Vec<u16> = parts
            .iter()
            .map(|part| self.part_height(part, inner))
            .collect();
        let total: u16 = heights.iter().fold(0, |sum, h| sum.saturating_add(*h));
        let max = total.saturating_sub(inner.height);
        self.max_scroll.set(max);
        self.view_height.set(inner.height);
        let scroll = self.scroll.min(max);
        let block = if max > 0 {
            block.title_bottom(Line::styled(
                format!(" {}% ", u32::from(scroll) * 100 / u32::from(max)),
                fg(p.muted),
            ))
        } else {
            block
        };
        frame.render_widget(block, area);
        let mut offset: u16 = 0;
        for (part, height) in parts.iter().zip(heights) {
            if let Some(target) = part.target() {
                self.placed.borrow_mut().push((target, offset, height));
            }
            // The part's rows on screen: those between the scroll and the
            // bottom, and how many of its own rows the top cuts off.
            let top = offset.max(scroll);
            let bottom = offset
                .saturating_add(height)
                .min(scroll.saturating_add(inner.height));
            if top < bottom {
                let shown = Rect::new(inner.x, inner.y + (top - scroll), inner.width, bottom - top);
                self.draw_part(frame, part, shown, top - offset, height, p);
            }
            offset = offset.saturating_add(height);
        }
    }

    /// The open issue as parts to lay out one under another.
    fn reading_parts(&self, issue: &Issue, p: &Palette) -> Vec<Part> {
        let mut parts = vec![Part::Text(vec![
            Line::styled(
                issue.title.clone(),
                fg(p.title).add_modifier(Modifier::BOLD),
            ),
            fields(issue, p),
            Line::default(),
        ])];
        let Some(detail) = self.detail() else {
            parts.push(Part::Text(vec![Line::styled("reading...", fg(p.muted))]));
            return parts;
        };
        let targets = self.targets();
        let links: Vec<(usize, &Link)> = targets
            .iter()
            .enumerate()
            .filter_map(|(at, t)| match t {
                Target::Issue(link) => Some((at, link)),
                Target::Image { .. } => None,
            })
            .collect();
        if !links.is_empty() {
            parts.push(Part::Text(vec![Line::styled(
                "Linked issues",
                fg(p.muted).add_modifier(Modifier::BOLD),
            )]));
            for (at, link) in links {
                parts.push(Part::Row(link_line(link, self.target == Some(at), p), at));
            }
            parts.push(Part::Text(vec![Line::default()]));
        }
        // An image is found among the targets by its URL, so Tab and a
        // click reach it.
        let image_at = |url: &str| {
            targets
                .iter()
                .position(|t| matches!(t, Target::Image { url: u, .. } if u == url))
        };
        let markdown_parts = |text: &str, parts: &mut Vec<Part>| {
            for piece in markdown::pieces(text, p) {
                match piece {
                    Piece::Lines(lines) => parts.push(Part::Text(lines)),
                    Piece::Image { url, alt } => match image_at(&url) {
                        Some(at) => parts.push(Part::Image {
                            target: at,
                            url,
                            alt,
                        }),
                        None => parts.push(Part::Text(vec![markdown::image_line(&alt, p)])),
                    },
                }
                parts.push(Part::Text(vec![Line::default()]));
            }
        };
        match detail
            .issue
            .description
            .as_deref()
            .filter(|d| !d.trim().is_empty())
        {
            Some(text) => markdown_parts(text, &mut parts),
            None => parts.push(Part::Text(vec![
                Line::styled("no description", fg(p.muted).add_modifier(Modifier::ITALIC)),
                Line::default(),
            ])),
        }
        if !detail.comments.is_empty() {
            parts.push(Part::Text(vec![Line::styled(
                format!("── {} comment(s) ", detail.comments.len()),
                fg(p.border),
            )]));
        }
        for comment in &detail.comments {
            parts.push(Part::Text(vec![
                Line::default(),
                Line::from(vec![
                    Span::styled(
                        comment.author.clone(),
                        fg(p.title).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("  {}", age(&comment.created_at)), fg(p.muted)),
                ]),
            ]));
            markdown_parts(&comment.body, &mut parts);
        }
        while matches!(parts.last(), Some(Part::Text(lines)) if lines.iter().all(|l| l.width() == 0))
        {
            parts.pop();
        }
        parts
    }

    /// The rows `part` takes in `inner`.
    fn part_height(&self, part: &Part, inner: Rect) -> u16 {
        match part {
            Part::Text(lines) => {
                let text = Paragraph::new(lines.clone()).wrap(Wrap { trim: false });
                u16::try_from(text.line_count(inner.width)).unwrap_or(u16::MAX)
            }
            Part::Row(..) => 1,
            // The picture, then a line naming it.
            Part::Image { url, .. } => match self.pictures.get(url) {
                Some(picture @ Picture::Ready { .. }) => {
                    picture
                        .fit(inner.width, image_rows(inner), INLINE_GROWTH, &self.images)
                        .1
                        + 1
                }
                _ => 1,
            },
        }
    }

    /// Draws the rows of `part` from `skip` on in `shown`; it is `height`
    /// rows in all.
    fn draw_part(
        &self,
        frame: &mut Frame,
        part: &Part,
        shown: Rect,
        skip: u16,
        height: u16,
        p: &Palette,
    ) {
        match part {
            Part::Text(lines) => frame.render_widget(
                Paragraph::new(lines.clone())
                    .wrap(Wrap { trim: false })
                    .scroll((skip, 0)),
                shown,
            ),
            Part::Row(line, target) => {
                frame.render_widget(Paragraph::new(line.clone()), shown);
                self.hits.borrow_mut().push((shown, *target));
            }
            Part::Image { target, url, alt } => {
                self.hits.borrow_mut().push((shown, *target));
                let rows = height - 1;
                let is_here = self.target == Some(*target);
                // The caption is the last row; the picture the ones above.
                if skip + shown.height > rows {
                    let caption = Rect::new(shown.x, shown.y + shown.height - 1, shown.width, 1);
                    frame
                        .render_widget(Paragraph::new(self.caption(url, alt, is_here, p)), caption);
                }
                let Some(Picture::Ready { protocol, .. }) = self.pictures.get(url) else {
                    return;
                };
                let picture_rows = rows.saturating_sub(skip).min(shown.height);
                if picture_rows == 0 {
                    return;
                }
                // Fitted to the rows it was given, it is as wide as then.
                let (columns, _) =
                    self.pictures[url].fit(shown.width, rows, INLINE_GROWTH, &self.images);
                let resize = if picture::is_cells(&self.images) {
                    Resize::Scale(None)
                } else {
                    Resize::Fit(None)
                };
                if skip == 0 && picture_rows == rows {
                    let area = Rect::new(shown.x, shown.y, columns, rows);
                    frame.render_stateful_widget(
                        StatefulImage::default().resize(resize),
                        area,
                        &mut *protocol.borrow_mut(),
                    );
                } else if picture::is_cells(&self.images) {
                    // Characters can be drawn whole aside and copied in
                    // part, so a picture scrolls a row at a time.
                    let mut aside = Buffer::empty(Rect::new(0, 0, columns, rows));
                    StatefulImage::default().resize(resize).render(
                        aside.area,
                        &mut aside,
                        &mut *protocol.borrow_mut(),
                    );
                    let buffer = frame.buffer_mut();
                    for row in 0..picture_rows {
                        for column in 0..columns {
                            buffer[(shown.x + column, shown.y + row)] =
                                aside[(column, skip + row)].clone();
                        }
                    }
                } else {
                    // A terminal's graphics cannot be drawn in part.
                    let area = Rect::new(shown.x, shown.y, shown.width, picture_rows);
                    frame.render_widget(
                        Paragraph::new(Line::styled(
                            "  scroll to see the picture whole",
                            fg(p.muted).add_modifier(Modifier::ITALIC),
                        )),
                        area,
                    );
                }
            }
        }
    }

    /// The line under a picture: its name, and how it is coming along.
    fn caption(&self, url: &str, alt: &str, is_here: bool, p: &Palette) -> Line<'static> {
        let mut spans = markdown::image_line(alt, p).spans;
        let note = match self.pictures.get(url) {
            None | Some(Picture::Loading) => "  loading...".to_string(),
            Some(Picture::Failed(err)) => format!("  could not show it: {err}"),
            Some(Picture::Ready { .. }) if is_here => {
                "  Enter shows it on the whole screen".to_string()
            }
            Some(Picture::Ready { .. }) => String::new(),
        };
        spans.push(Span::styled(note, fg(p.muted)));
        let line = Line::from(spans);
        if is_here { line.style(p.cursor) } else { line }
    }

    /// Image `at` of the open issue over the whole of `area`.
    fn draw_viewer(&self, frame: &mut Frame, area: Rect, at: usize, p: &Palette) {
        let targets = self.targets();
        let images: Vec<usize> = targets
            .iter()
            .enumerate()
            .filter(|(_, t)| matches!(t, Target::Image { .. }))
            .map(|(i, _)| i)
            .collect();
        let Some(Target::Image { url, alt }) = targets.get(at) else {
            return;
        };
        let name = if alt.trim().is_empty() {
            "image"
        } else {
            alt.trim()
        };
        let place = images.iter().position(|i| *i == at).map_or(1, |n| n + 1);
        let block = Block::bordered()
            .border_style(fg(p.accent))
            .title(Span::styled(
                format!(" {name} · {place} of {} ", images.len()),
                fg(p.accent).add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        match self.pictures.get(url) {
            Some(picture @ Picture::Ready { protocol, .. }) => {
                let (columns, rows) =
                    picture.fit(inner.width, inner.height, f64::MAX, &self.images);
                let centered = Rect::new(
                    inner.x + (inner.width - columns) / 2,
                    inner.y + (inner.height - rows) / 2,
                    columns,
                    rows,
                );
                let resize = if picture::is_cells(&self.images) {
                    Resize::Scale(None)
                } else {
                    Resize::Fit(None)
                };
                frame.render_stateful_widget(
                    StatefulImage::default().resize(resize),
                    centered,
                    &mut *protocol.borrow_mut(),
                );
            }
            Some(Picture::Failed(err)) => frame.render_widget(
                Paragraph::new(format!("could not show it: {err}")).style(fg(p.danger)),
                inner,
            ),
            _ => frame.render_widget(Paragraph::new("loading...").style(fg(p.muted)), inner),
        }
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
        // The bar above the list says what it shows; this, which view.
        right.push(Span::styled(
            format!("  {}", self.view_name()),
            fg(p.accent),
        ));
        if self.is_loading {
            right.push(Span::styled("  loading...", fg(p.info)));
        }
        // The list's count and view say nothing about the issue in the
        // form, and its keys need the room.
        if self.form.is_some() {
            right.clear();
        }
        right.push(Span::raw(" "));
        right.push(Span::styled(" treetop · issues ", p.badge));
        let right = Line::from(right);
        let right_width = u16::try_from(right.width())
            .unwrap_or(u16::MAX)
            .min(area.width);
        let [left_area, right_area] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(right_width)]).areas(area);
        let left = if self.search.is_some() {
            Line::from(key_bar(
                &[("Enter", "keep it"), ("Esc", "clear it"), ("↑ ↓", "move")],
                left_area.width,
                p,
            ))
        } else if self.panel.is_some() && self.picker.is_none() {
            Line::from(key_bar(
                &[
                    ("Enter", "change"),
                    ("x", "clear"),
                    ("c", "clear all"),
                    ("Esc", "close"),
                ],
                left_area.width,
                p,
            ))
        } else if self.choosing.is_some() && self.message.is_none() {
            Line::from(key_bar(
                &[
                    ("Enter", "choose"),
                    ("/", "search"),
                    ("f", "filters"),
                    ("→", "read"),
                    ("Esc", "cancel"),
                ],
                left_area.width,
                p,
            ))
        } else if let Some(message) = &self.message {
            Line::styled(format!(" {message}"), fg(p.warning))
        } else if let (Some(err), false) = (&self.error, self.issues.is_empty()) {
            Line::styled(format!(" {err}"), fg(p.danger))
        } else if let Some(form) = &self.form {
            Line::from(key_bar(form_keys(form.field), left_area.width, p))
        } else if self.focus == Focus::Issue && self.viewer.is_some() {
            Line::from(key_bar(&VIEWER_KEYS, left_area.width, p))
        } else if self.focus == Focus::Issue {
            Line::from(key_bar(&ISSUE_KEYS, left_area.width, p))
        } else {
            Line::from(key_bar(&KEYS, left_area.width, p))
        };
        frame.render_widget(Paragraph::new(left).style(p.bar), left_area);
        frame.render_widget(Paragraph::new(right).style(p.bar), right_area);
    }
}

/// A piece of the open issue, laid out one under another.
enum Part {
    /// Lines, wrapped to the width.
    Text(Vec<Line<'static>>),
    /// One line, not wrapped, that leads to target `.1`.
    Row(Line<'static>, usize),
    Image {
        target: usize,
        url: String,
        alt: String,
    },
}

impl Part {
    fn target(&self) -> Option<usize> {
        match self {
            Part::Text(_) => None,
            Part::Row(_, target) | Part::Image { target, .. } => Some(*target),
        }
    }
}

/// How much bigger than its own size an image in the text grows, drawn in
/// characters: enough to read a screenshot, not so much an icon fills the
/// screen. On the whole screen an image grows as large as fits.
const INLINE_GROWTH: f64 = 2.0;

/// The most rows an image in the text takes, so the text around it shows.
fn image_rows(inner: Rect) -> u16 {
    inner.height.saturating_sub(4).max(6)
}

/// A linked issue's row: how it is linked, its id, title and status, the
/// one Tab is on marked.
fn link_line(link: &Link, is_here: bool, p: &Palette) -> Line<'static> {
    let mark = if is_here { "› " } else { "  " };
    let line = Line::from(vec![
        Span::styled(format!("{mark}{:<14}", link.kind), fg(p.muted)),
        Span::styled(format!("{:<10}", link.id), fg(p.accent)),
        Span::raw(link.title.clone()),
        Span::styled(
            if link.state.is_empty() {
                String::new()
            } else {
                format!("  {}", link.state)
            },
            fg(p.info),
        ),
    ]);
    if is_here { line.style(p.cursor) } else { line }
}

/// The form over the whole of `area`: the title, the fields the pickers
/// set, and the description below, the field the cursor is in marked.
/// `has_cursor` puts the terminal's cursor in it, unless a picker is over it.
fn draw_form(
    frame: &mut Frame,
    area: Rect,
    form: &Form,
    team: &str,
    p: &Palette,
    has_cursor: bool,
) {
    let heading = match form.id() {
        Some(id) => format!(" Edit {id} "),
        None if team.is_empty() => " New issue ".to_string(),
        None => format!(" New issue in {team} "),
    };
    let block = Block::bordered()
        .border_style(fg(p.accent))
        .padding(Padding::new(2, 2, 1, 0))
        .title(Span::styled(
            heading,
            fg(p.accent).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // The title, a gap, a row per picked field, a gap, the description's
    // heading, and its box below.
    let picked = FIELDS.len() - 2;
    let [title_area, _, fields_area, _, heading_area, text_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(u16::try_from(picked).unwrap_or(5)),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(inner);
    const LABEL: u16 = 12;
    let label = |field: Field| {
        let is_here = form.field == field;
        let style = if is_here {
            fg(p.accent).add_modifier(Modifier::BOLD)
        } else {
            fg(p.muted)
        };
        let mark = if is_here { "› " } else { "  " };
        Span::styled(format!("{mark}{}", field.label()), style)
    };
    // Each picked value is padded a column each side, so the title starts a
    // column in too, to line up.
    let [title_label, title_value] =
        Layout::horizontal([Constraint::Length(LABEL + 1), Constraint::Fill(1)]).areas(title_area);
    frame.render_widget(Paragraph::new(Line::from(label(Field::Title))), title_label);
    let title = form.title.text();
    if title.is_empty() && form.field != Field::Title {
        frame.render_widget(
            Paragraph::new("untitled").style(fg(p.muted).add_modifier(Modifier::ITALIC)),
            title_value,
        );
    } else {
        // A title longer than the row shows its end, where the cursor is.
        let (_, col) = form.title.cursor(usize::MAX);
        let room = usize::from(title_value.width.saturating_sub(1)).max(1);
        let skip = col.saturating_sub(room);
        let shown: String = title.chars().skip(skip).collect();
        frame.render_widget(
            Paragraph::new(shown).style(Style::new().add_modifier(Modifier::BOLD)),
            title_value,
        );
        if form.field == Field::Title && has_cursor {
            let x = u16::try_from(col - skip).unwrap_or(0);
            frame.set_cursor_position((title_value.x + x, title_value.y));
        }
    }
    let issue = &form.issue;
    let values: Vec<(Field, Option<String>, Style)> = vec![
        (
            Field::Status,
            Some(issue.state.clone()).filter(|s| !s.is_empty()),
            fg(p.info),
        ),
        (
            Field::Priority,
            (issue.priority > 0).then(|| issue.priority_label().to_string()),
            fg(p.warning),
        ),
        (
            Field::Labels,
            Some(issue.labels.join(", ")).filter(|l| !l.is_empty()),
            fg(p.special),
        ),
        (Field::Assignee, issue.assignee.clone(), fg(p.text)),
        (Field::Project, issue.project.clone(), fg(p.text)),
    ];
    let rows: Vec<Line> = values
        .into_iter()
        .map(|(field, value, style)| {
            let empty = match field {
                Field::Assignee => "unassigned",
                Field::Project => "no project",
                Field::Labels => "no labels",
                Field::Priority => "no priority",
                _ => "none",
            };
            let (text, style) = match value {
                Some(text) => (text, style),
                None => (empty.to_string(), fg(p.muted)),
            };
            let mut spans = vec![
                label(field),
                Span::raw(" ".repeat(usize::from(LABEL).saturating_sub(field.label().len() + 2))),
            ];
            if form.field == field {
                spans.push(Span::styled(format!(" {text} "), p.cursor));
                spans.push(Span::styled("  Enter changes it", fg(p.muted)));
            } else {
                spans.push(Span::styled(format!(" {text} "), style));
            }
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), fields_area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            label(Field::Description),
            Span::styled("   Markdown; Ctrl+E writes it in $EDITOR", fg(p.muted)),
        ])),
        heading_area,
    );
    let is_here = form.field == Field::Description;
    let text_block = Block::bordered()
        .border_style(fg(if is_here { p.accent } else { p.border }))
        .padding(Padding::horizontal(1));
    let text_inner = text_block.inner(text_area);
    frame.render_widget(text_block, text_area);
    // One column is kept for the cursor at the end of a full row.
    let width = text_inner.width.saturating_sub(1).max(1);
    form.width.set(width);
    let height = usize::from(text_inner.height).max(1);
    if form.description.is_empty() && !is_here {
        frame.render_widget(
            Paragraph::new("no description").style(fg(p.muted).add_modifier(Modifier::ITALIC)),
            text_inner,
        );
        return;
    }
    let rows = form.description.rows(usize::from(width));
    let (row, col) = form.description.cursor(usize::from(width));
    // The rows shown follow the cursor, moving no more than they must.
    let mut top = form.top.get().min(rows.len().saturating_sub(1));
    if row < top {
        top = row;
    } else if row >= top + height {
        top = row + 1 - height;
    }
    form.top.set(top);
    let lines: Vec<Line> = rows
        .iter()
        .skip(top)
        .take(height)
        .map(|(line, start, end)| {
            let text: String = form.description.lines()[*line]
                .chars()
                .skip(*start)
                .take(end - start)
                .collect();
            Line::raw(text)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), text_inner);
    if is_here && has_cursor {
        let y = u16::try_from(row - top).unwrap_or(0);
        let x = u16::try_from(col).unwrap_or(0);
        frame.set_cursor_position((text_inner.x + x, text_inner.y + y));
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
            match picker.kind {
                Kind::Choice => format!(" {} ", picker.issue),
                kind => format!(" {} of {} ", kind.noun(), picker.issue),
            },
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

/// The comment box over the lower half: the text, wrapped, the end of a
/// long one kept in view, and the keys.
fn draw_compose(frame: &mut Frame, area: Rect, compose: &Compose, p: &Palette) {
    let width = area.width.saturating_sub(8).min(100);
    let inner_width = usize::from(width.saturating_sub(2)).max(1);
    let mut rows: Vec<String> = Vec::new();
    for line in compose.text.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            rows.push(String::new());
        }
        rows.extend(chars.chunks(inner_width).map(|c| c.iter().collect()));
    }
    let shown = rows.len().clamp(6, 14);
    let height = u16::try_from(shown + 2)
        .unwrap_or(u16::MAX)
        .min(area.height);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let block = Block::bordered()
        .border_style(fg(p.accent))
        .title(Span::styled(
            format!(" Comment on {} ", compose.issue),
            fg(p.title).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::styled(
            " Enter sends  Alt+Enter new line  Esc cancels ",
            fg(p.muted),
        ));
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    if compose.text.is_empty() {
        frame.render_widget(
            Paragraph::new("Markdown; mention people with @name")
                .style(fg(p.muted).add_modifier(Modifier::ITALIC)),
            inner,
        );
        frame.set_cursor_position((inner.x, inner.y));
        return;
    }
    let first = rows.len().saturating_sub(usize::from(inner.height));
    let lines: Vec<Line> = rows[first..].iter().map(|r| Line::raw(r.clone())).collect();
    let last = rows.last().map_or(0, |r| r.chars().count());
    let cursor_row = u16::try_from(rows.len() - 1 - first).unwrap_or(0);
    frame.render_widget(Paragraph::new(lines), inner);
    frame.set_cursor_position((
        inner.x
            + u16::try_from(last)
                .unwrap_or(0)
                .min(inner.width.saturating_sub(1)),
        inner.y + cursor_row,
    ));
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
                    format!(" {key:<10} "),
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
