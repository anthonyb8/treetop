//! Drawing the Issues screen: the list on top, the issue under the cursor
//! below with its description and comments, and the bar at the bottom. An
//! open issue takes the whole screen. On the program's panels the list and
//! the issue are drawn into the boxes it lays out, from the same pieces.

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
use crate::host::{Host, Work};
use crate::markdown::{self, Piece};
use crate::panel::{self, ROWS};
use crate::picker::{Kind, Picker};
use crate::picture::{self, Picture};
use crate::screen::{Focus, Screen, issues_said};
use crate::targets::Target;
use crate::views::Group;

/// The bar's keys, most used first; `?` lists them all.
const KEYS: [(&str, &str); 6] = [
    ("Enter", "read"),
    ("p", "pick up"),
    ("z", "fold"),
    ("/", "search"),
    ("Tab", "trees"),
    ("?", "help"),
];

/// The bar's keys on the list while issues are marked.
const MARKED_KEYS: [(&str, &str); 6] = [
    ("s", "status"),
    ("a", "assignee"),
    ("P", "project"),
    ("Esc", "unmark"),
    ("x", "mark"),
    ("X", "all"),
];

/// The bar's keys on the panels, with [2] the list's.
const PANEL_LIST_KEYS: [(&str, &str); 11] = [
    ("j/k", "move"),
    ("Enter", "read it"),
    ("p", "pick up"),
    ("f", "filters"),
    ("/", "search"),
    ("s", "status"),
    ("x", "mark"),
    ("1", "issue"),
    ("3", "project"),
    ("Tab", "Trees"),
    ("?", "help"),
];

/// The bar's keys on the panels, with [3] the project's.
const PANEL_PROJECT_KEYS: [(&str, &str); 8] = [
    ("j/k", "move"),
    ("Enter", "show it"),
    ("p", "pick up"),
    ("1", "issue"),
    ("2", "list"),
    ("Esc", "back"),
    ("Tab", "Trees"),
    ("?", "help"),
];

/// The bar's keys on the panels, with [1] the issue's.
const PANEL_ISSUE_KEYS: [(&str, &str); 10] = [
    ("j/k", "scroll"),
    ("Tab", "links"),
    ("p", "pick up"),
    ("c", "comment"),
    ("e", "edit"),
    ("o", "browser"),
    ("Esc", "back"),
    ("2", "list"),
    ("3", "project"),
    ("?", "help"),
];

/// The bar's keys inside an open issue.
const ISSUE_KEYS: [(&str, &str); 5] = [
    ("Esc", "back"),
    ("Tab", "links"),
    ("p", "pick up"),
    ("o", "open"),
    ("?", "help"),
];

/// The bar's keys over an image on the whole screen.
const VIEWER_KEYS: [(&str, &str); 3] = [
    ("Esc", "back"),
    ("← →", "other images"),
    ("o", "open it in the browser"),
];

/// The bar's keys in the comment box.
const COMPOSE_KEYS: [(&str, &str); 3] = [
    ("Enter", "send"),
    ("Alt+Enter", "new line"),
    ("Esc", "cancel"),
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
const HELP: [(&str, &str); 28] = [
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
    (
        "s",
        "status: pick one, typing narrows the list; marked, every one's",
    ),
    ("l", "labels: Tab marks or unmarks each, Enter applies them"),
    (
        "a",
        "assignee: you, nobody, or a teammate; marked, every one's",
    ),
    (
        "P",
        "project: one of the team's, or none; marked, every one's",
    ),
    (
        "x X",
        "mark the issue and move down, or every one shown; Esc unmarks",
    ),
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
        "filters: assignee, creator, status, priority, label, project, cycle, grouping",
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
    (
        "[ ] w",
        "the issue source before or after, work and personal; w the next",
    ),
    ("o", "open it in the browser"),
    ("r", "read the list and the team again"),
    (
        "Tab Enter",
        "in an issue: move to a link or image, and open it; Esc back",
    ),
    ("j k ↑ ↓", "move, or scroll the issue you are reading"),
    ("Space PgUp", "a page down or up in the issue"),
    ("g G", "top, bottom"),
    (
        ":",
        "ask the assistant about this issue; it answers in its chat",
    ),
    ("Tab", "back to the trees"),
    ("q", "quit"),
];

/// What the bar says on the left: the keys for what is on screen, or a
/// message or error in their place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bar<'a> {
    Keys(&'static [(&'static str, &'static str)]),
    Message(&'a str),
    Error(&'a str),
}

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

/// The issue's priority, in a word, or cut to four letters in the panels'
/// narrow column.
fn priority(issue: &Issue, p: &Palette, is_short: bool) -> Cell<'static> {
    let text = match issue.priority {
        1 if is_short => "urg",
        3 if is_short => "med",
        1..=4 => issue.priority_label(),
        _ => "",
    };
    Cell::from(text).style(priority_style(issue, p))
}

/// An urgent issue loud, a high one in the warning colour, the rest quieter.
fn priority_style(issue: &Issue, p: &Palette) -> Style {
    match issue.priority {
        1 => fg(p.danger).add_modifier(Modifier::BOLD),
        2 => fg(p.warning),
        3 => fg(p.text),
        _ => fg(p.muted),
    }
}

/// Which tree is working on the issue, and its agent's state, as
/// `work_span` says it.
fn work_cell(host: &dyn Host, id: &str, p: &Palette) -> Cell<'static> {
    host.work_on(id)
        .map_or_else(|| Cell::from(""), |work| Cell::from(work_span(&work, p)))
}

/// The work on an issue as the WORK column says it, for the program's own
/// panels to say it the same: `4 · waiting` loud, a busy one green, the
/// rest muted, or `ready`.
pub fn work_span(work: &Work, p: &Palette) -> Span<'static> {
    let text = if work.is_ready {
        "ready".to_string()
    } else {
        let agent = work.agent.as_deref().unwrap_or("no agent");
        format!("{} · {agent}", work.tree)
    };
    Span::styled(text, work_style(work, p))
}

/// The work on an issue in words, for [1] on the panels: its tree and
/// agent, whether it is ready, or that nobody has it yet.
fn work_said(host: &dyn Host, id: &str, p: &Palette) -> Span<'static> {
    let Some(work) = host.work_on(id) else {
        return Span::styled("nobody yet: p picks it up", fg(p.muted));
    };
    let text = if work.is_ready {
        "ready to pick up: nothing blocks it".to_string()
    } else {
        let agent = work.agent.as_deref().unwrap_or("none");
        format!("tree {}, agent {agent}", work.tree)
    };
    Span::styled(text, work_style(&work, p))
}

/// What needs you loud, a busy agent or a ready issue green, the rest
/// muted.
fn work_style(work: &Work, p: &Palette) -> Style {
    if work.is_ready {
        fg(p.success).add_modifier(Modifier::BOLD)
    } else if work.needs_you {
        fg(p.warning).add_modifier(Modifier::BOLD)
    } else if work.agent.as_deref() == Some("busy") {
        fg(p.success)
    } else {
        fg(p.muted)
    }
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
            let block = Block::bordered()
                .border_style(fg(p.accent))
                .padding(Padding::new(2, 2, 1, 0))
                .title(Span::styled(
                    format!(" {} ", self.form_title().unwrap_or_default()),
                    fg(p.accent).add_modifier(Modifier::BOLD),
                ));
            draw_form(frame, block.inner(body), form, p, self.picker.is_none());
            frame.render_widget(block, body);
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
            self.draw_list(frame, list, p, host, false);
            self.draw_detail(frame, detail, p);
            self.draw_footer(frame, footer, p);
        }
        self.draw_over(frame, area, area, p);
        if let Some(compose) = &self.compose {
            draw_compose(frame, area, compose, p);
        }
    }

    /// What is open over the screen: the help and the filters over the
    /// whole of `area`, and a picker, with the filters' own over them too,
    /// in the middle of `window`, [1] on the panels.
    pub fn draw_over(&self, frame: &mut Frame, area: Rect, window: Rect, p: &Palette) {
        if self.is_help {
            draw_help(frame, area, p);
        } else if let (Some(at), None) = (self.panel, &self.picker) {
            self.draw_panel(frame, area, at, p);
        } else if let Some(picker) = &self.picker {
            let on = if self.panel.is_some() { area } else { window };
            draw_picker(frame, on, picker, p);
        }
    }

    /// The screen takes the whole of its area even on the panels: an image
    /// filling it.
    pub fn is_full_screen(&self) -> bool {
        self.viewer.is_some()
    }

    /// The form or the comment box is open, so what is typed goes there.
    pub fn is_writing(&self) -> bool {
        self.form.is_some() || self.compose.is_some()
    }

    /// The open form's title: the issue it edits, or the team a new one
    /// goes to.
    pub fn form_title(&self) -> Option<String> {
        let form = self.form.as_ref()?;
        let team = self.teams.get(&self.source).map(|t| t.key.as_str());
        Some(match (form.id(), team) {
            (Some(id), _) => format!("Edit {id}"),
            (None, Some(team)) if !team.is_empty() => format!("New issue in {team}"),
            (None, _) => "New issue".to_string(),
        })
    }

    /// [2]'s body on the panels: the filter line, and the list in the
    /// columns that fit beside [1].
    pub fn draw_list_panel(&self, frame: &mut Frame, area: Rect, p: &Palette, host: &dyn Host) {
        self.draw_list(frame, area, p, host, true);
    }

    /// [2]'s title after its tabs of sources: how many issues it lists,
    /// how many are marked, and whether it is reading them, each its own
    /// span so a narrow title can give up the last.
    pub fn list_title(&self, p: &Palette) -> Vec<Span<'static>> {
        // Without a source of its own the filter line says so, and lists
        // nothing to count.
        if self.is_without_source {
            return Vec::new();
        }
        let mut spans = vec![Span::styled(
            format!("· {} ", issues_said(self.issues.len())),
            fg(p.muted),
        )];
        if !self.marked.is_empty() {
            spans.push(Span::styled(
                format!("· {} marked ", self.marked.len()),
                fg(p.success).add_modifier(Modifier::BOLD),
            ));
        }
        if self.is_loading {
            spans.push(Span::styled("· loading... ", fg(p.info)));
        }
        spans
    }

    /// [1]'s title after its number: the issues opened to get here, the one
    /// shown marked, and its title.
    pub fn issue_title(&self, p: &Palette) -> Vec<Span<'static>> {
        let mut spans = self.trail_spans(p);
        if let Some(issue) = self.current() {
            spans.push(Span::raw(format!(" {} ", issue.title)));
        } else if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        spans
    }

    /// How far down the issue shown is, for the bottom of its box, once it
    /// is longer than the box.
    pub fn issue_scrolled(&self, p: &Palette) -> Option<Line<'static>> {
        let max = self.max_scroll.get();
        (max > 0 && self.current().is_some()).then(|| {
            Line::styled(
                format!(
                    " {}% ",
                    u32::from(self.scroll.min(max)) * 100 / u32::from(max)
                ),
                fg(p.muted),
            )
        })
    }

    /// The bar's left, by what is on screen: the keys, or the message or
    /// error there is in their place.
    pub fn bar(&self) -> Bar<'_> {
        if self.search.is_some() {
            return Bar::Keys(&[("Enter", "keep it"), ("Esc", "clear it"), ("↑ ↓", "move")]);
        }
        if self.panel.is_some() && self.picker.is_none() {
            return Bar::Keys(&[
                ("Enter", "change"),
                ("x", "clear"),
                ("c", "clear all"),
                ("Esc", "close"),
            ]);
        }
        if self.choosing.is_some() && self.message.is_none() {
            return Bar::Keys(&[
                ("Enter", "choose"),
                ("/", "search"),
                ("f", "filters"),
                ("→", "read"),
                ("Esc", "cancel"),
            ]);
        }
        if let Some(message) = &self.message {
            return Bar::Message(message);
        }
        if let (Some(err), false) = (&self.error, self.issues.is_empty()) {
            return Bar::Error(err);
        }
        Bar::Keys(match &self.form {
            Some(form) => form_keys(form.field),
            // Off the panels the comment popup lists its keys on its own border.
            None if self.compose.is_some() && self.is_panels => &COMPOSE_KEYS,
            None if self.focus == Focus::Issue && self.viewer.is_some() => &VIEWER_KEYS,
            None if self.focus == Focus::Issue && self.is_panels => &PANEL_ISSUE_KEYS,
            None if self.focus == Focus::Issue => &ISSUE_KEYS,
            None if self.focus == Focus::Project => &PANEL_PROJECT_KEYS,
            None if !self.marked.is_empty() => &MARKED_KEYS,
            None if self.is_panels => &PANEL_LIST_KEYS,
            None => &KEYS,
        })
    }

    /// The list, the filter line above it: every column on the screen of
    /// its own, or, `is_compact`, the four that fit beside [1] on the panels.
    fn draw_list(
        &self,
        frame: &mut Frame,
        area: Rect,
        p: &Palette,
        host: &dyn Host,
        is_compact: bool,
    ) {
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
        // Grouped by status, each heading already says it, so the column
        // would only repeat it; the compact list keeps ID, PRI, TITLE and
        // WORK, the issue's other fields being in [1] beside it.
        let has_status = self.group != Group::Status;
        let is_shown = |at: usize| match at {
            _ if is_compact => matches!(at, 0 | 2 | 3 | 6),
            1 => has_status,
            _ => true,
        };
        let columns = |cells: Vec<Cell<'static>>| -> Vec<Cell<'static>> {
            cells
                .into_iter()
                .enumerate()
                .filter(|(at, _)| is_shown(*at))
                .map(|(_, cell)| cell)
                .collect()
        };
        let priority_heading = if is_compact { "PRI" } else { "PRIORITY" };
        let header = Row::new(columns(
            [
                "ID",
                "STATUS",
                priority_heading,
                "TITLE",
                "LABELS",
                "ASSIGNEE",
                "WORK",
            ]
            .map(Cell::from)
            .to_vec(),
        ))
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
                rows.push(Row::new(columns(vec![
                    Cell::from(glyph).style(fg(p.accent)),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(Line::from(vec![
                        Span::styled(heading, fg(p.title).add_modifier(Modifier::BOLD)),
                        Span::styled(format!("  {count}"), fg(p.muted)),
                    ])),
                ])));
            }
            if is_folded {
                continue;
            }
            if at == self.selected {
                cursor_row = rows.len();
            }
            // While any are marked, each row says whether it is.
            let id = match (self.marked.is_empty(), self.marked.contains(&issue.id)) {
                (true, _) => Cell::from(issue.id.clone()).style(fg(p.accent)),
                (false, true) => Cell::from(Line::from(vec![
                    Span::styled("✓ ", fg(p.success).add_modifier(Modifier::BOLD)),
                    Span::styled(issue.id.clone(), fg(p.accent).add_modifier(Modifier::BOLD)),
                ])),
                (false, false) => Cell::from(format!("  {}", issue.id)).style(fg(p.accent)),
            };
            rows.push(Row::new(columns(vec![
                id,
                Cell::from(issue.state.clone()).style(fg(p.info)),
                priority(issue, p, is_compact),
                Cell::from(issue.title.clone()),
                Cell::from(issue.labels.join(", ")).style(fg(p.special)),
                Cell::from(issue.assignee.clone().unwrap_or_default()).style(fg(p.muted)),
                work_cell(host, &issue.id, p),
            ])));
        }
        // The compact list gives the title what it can: an id of eight,
        // `urg`, and `12 · waiting` are as wide as its columns go.
        let id_width = match (is_compact, self.marked.is_empty()) {
            (true, true) => 9,
            (true, false) => 11,
            (false, true) => 10,
            (false, false) => 12,
        };
        let widths: Vec<Constraint> = [
            Constraint::Length(id_width),
            Constraint::Length(14),
            Constraint::Length(if is_compact { 4 } else { 8 }),
            Constraint::Fill(1),
            Constraint::Length(18),
            Constraint::Length(14),
            Constraint::Length(if is_compact { 12 } else { 14 }),
        ]
        .into_iter()
        .enumerate()
        .filter(|(at, _)| is_shown(*at))
        .map(|(_, width)| width)
        .collect();
        let table = Table::new(rows, widths)
            .header(header)
            .row_highlight_style(p.cursor);
        let mut state = TableState::default().with_selected(Some(cursor_row));
        // In [2]'s box the table keeps a column of air each side, as the
        // filter line above it and the other panels' text do.
        let area = if is_compact {
            Block::new().padding(Padding::horizontal(1)).inner(area)
        } else {
            area
        };
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
        let hint = Line::from(vec![
            Span::styled("f", fg(p.accent)),
            Span::styled(" filters  ", fg(p.muted)),
            Span::styled("/", fg(p.accent)),
            Span::styled(" search ", fg(p.muted)),
        ]);
        let hint_width = u16::try_from(hint.width()).unwrap_or(0);
        let pieces: Vec<(String, Style)> = panel::describe(&self.filters, self.group)
            .into_iter()
            .enumerate()
            .map(|(at, piece)| {
                let style = if at == 0 {
                    fg(p.title).add_modifier(Modifier::BOLD)
                } else {
                    fg(p.info)
                };
                (piece, style)
            })
            .collect();
        let sep = "  ·  ";
        let width = |text: &str| Span::raw(text).width();
        let search = self.search_text().map(|search| format!("“{search}”"));
        // A narrow list drops the filters that do not fit whole, so none is
        // cut mid-word and two spaces always part them from the hints. The
        // search narrows the list most, so it is kept over the filters
        // before it, and cut itself only when nothing else is left to drop.
        let room = usize::from(area.width.saturating_sub(hint_width)).saturating_sub(2);
        let search_width = search.as_deref().map_or(0, |s| width(sep) + width(s));
        let filter_room = room.saturating_sub(search_width);
        let mut spans = vec![Span::raw(" ")];
        let mut used = 1;
        let last = pieces.len().saturating_sub(1);
        for (at, (piece, style)) in pieces.into_iter().enumerate() {
            let piece_width = if at > 0 { width(sep) } else { 0 } + width(&piece);
            // Any piece but the last keeps room for the ellipsis after it.
            let ellipsis = if at == last { 0 } else { 2 };
            if at > 0 && used + piece_width + ellipsis > filter_room {
                if used + 2 <= filter_room {
                    spans.push(Span::styled(" …", fg(p.muted)));
                    used += 2;
                }
                break;
            }
            if at > 0 {
                spans.push(Span::styled(sep, fg(p.border)));
            }
            spans.push(Span::styled(piece, style));
            used += piece_width;
        }
        if let Some(search) = search {
            let left = room.saturating_sub(used + width(sep));
            let search = if width(&search) <= left {
                search
            } else {
                let mut cut: String = String::new();
                for c in search.chars() {
                    if width(&cut) + width(c.encode_utf8(&mut [0; 4])) + 1 > left {
                        break;
                    }
                    cut.push(c);
                }
                format!("{cut}…")
            };
            spans.push(Span::styled(sep, fg(p.border)));
            spans.push(Span::styled(search, fg(p.special)));
        }
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
            match self.folded_note() {
                Some(text) => frame.render_widget(
                    Paragraph::new(Line::styled(text, fg(p.muted))).block(block),
                    area,
                ),
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

    /// The heading of a folded group under the cursor, which stands for
    /// its issues: how many it has, and how to open it.
    fn folded_note(&self) -> Option<String> {
        let group = self
            .group_of(self.selected)
            .filter(|_| self.is_folded(self.selected))?;
        let count = (0..self.issues.len())
            .filter(|at| self.group_of(*at).as_ref() == Some(&group))
            .count();
        Some(format!(
            "{group}: {count} issue(s), folded; z or Enter opens it"
        ))
    }

    /// The issues read to get to the one shown: where it was opened from,
    /// then each issue opened from a link, the one shown last and marked;
    /// one opened from the trees starts the trail itself.
    fn trail_spans(&self, p: &Palette) -> Vec<Span<'static>> {
        let mut trail: Vec<&str> = self
            .issues
            .get(self.selected)
            .filter(|_| !self.is_from_outside && !self.is_from_project)
            .map(|i| i.id.as_str())
            .into_iter()
            .collect();
        trail.extend(self.trail.iter().map(String::as_str));
        let last = trail.len().saturating_sub(1);
        trail
            .iter()
            .enumerate()
            .map(|(at, id)| {
                if at == last {
                    Span::styled(id.to_string(), fg(p.accent).add_modifier(Modifier::BOLD))
                } else {
                    Span::styled(format!("{id} › "), fg(p.muted))
                }
            })
            .collect()
    }

    /// The open issue over the whole of `area`, in its own box.
    fn draw_reading(&self, frame: &mut Frame, area: Rect, p: &Palette) {
        let mut title = vec![Span::raw(" ")];
        title.extend(self.trail_spans(p));
        title.push(Span::raw(" "));
        let block = Block::bordered()
            .border_style(fg(p.accent))
            .padding(Padding::horizontal(1))
            .title(Line::from(title));
        self.draw_text(frame, block.inner(area), p, None);
        // Its bottom says how far down the text, once it is drawn, is.
        let block = match self.issue_scrolled(p) {
            Some(line) => block.title_bottom(line),
            None => block,
        };
        frame.render_widget(block, area);
    }

    /// [1]'s body on the panels: the issue under the cursor, or one opened
    /// from its links, whole, its fields a row each with its work from
    /// `host`, the comment box under it while one is written; or the form,
    /// while it is open.
    pub fn draw_issue(&self, frame: &mut Frame, inner: Rect, p: &Palette, host: &dyn Host) {
        if let Some(form) = &self.form {
            // A row of air under [1]'s title, as the form has on a screen
            // of its own, and over [1]'s bottom, so the description's box
            // does not sit on it.
            let body = Block::new().padding(Padding::vertical(1)).inner(inner);
            return draw_form(frame, body, form, p, self.picker.is_none());
        }
        let Some(compose) = &self.compose else {
            return self.draw_text(frame, inner, p, Some(host));
        };
        // The comment box docks under the issue it is on, which scrolls in
        // what is left above it.
        let height = compose_height(compose, inner.width).min(inner.height);
        let [text, docked] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(height)]).areas(inner);
        self.draw_text(frame, text, p, Some(host));
        draw_compose_box(frame, docked, compose, p, false);
    }

    /// The issue shown, in `inner`: its title and fields, its links, its
    /// description with its images, and its comments, scrolled. With a
    /// `host` it is headed as [1] on the panels, whose box has its title.
    fn draw_text(&self, frame: &mut Frame, inner: Rect, p: &Palette, host: Option<&dyn Host>) {
        self.issue_area.set(inner);
        self.hits.borrow_mut().clear();
        self.placed.borrow_mut().clear();
        let Some(issue) = self.current() else {
            self.max_scroll.set(0);
            let text = if self.trail.is_empty()
                && let Some(note) = self.folded_note()
            {
                note
            } else if let Some(id) = self.viewed_id() {
                format!("reading {id}...")
            } else if self.is_loading {
                // Until the list is read, [2] says it is being read.
                String::new()
            } else {
                "no issue under the cursor".to_string()
            };
            frame.render_widget(Paragraph::new(text).style(fg(p.muted)), inner);
            return;
        };
        let parts = self.reading_parts(issue, p, host);
        let heights: Vec<u16> = parts
            .iter()
            .map(|part| self.part_height(part, inner))
            .collect();
        let total: u16 = heights.iter().fold(0, |sum, h| sum.saturating_add(*h));
        let max = total.saturating_sub(inner.height);
        self.max_scroll.set(max);
        self.view_height.set(inner.height);
        let scroll = self.scroll.min(max);
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

    /// The open issue as parts to lay out one under another, headed by its
    /// title and fields, or with a `host` by a row for each field.
    fn reading_parts(&self, issue: &Issue, p: &Palette, host: Option<&dyn Host>) -> Vec<Part> {
        let mut parts = match host {
            None => vec![Part::Text(vec![
                Line::styled(
                    issue.title.clone(),
                    fg(p.title).add_modifier(Modifier::BOLD),
                ),
                fields(issue, p),
                Line::default(),
            ])],
            Some(host) => vec![
                Part::Text(field_rows(issue, host, p)),
                Part::Rule,
                Part::Text(vec![Line::default()]),
            ],
        };
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
            Part::Row(..) | Part::Rule => 1,
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
            Part::Rule => frame.render_widget(
                Paragraph::new("─".repeat(usize::from(shown.width))).style(fg(p.border)),
                shown,
            ),
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
        if !self.marked.is_empty() {
            right.push(Span::styled(
                format!("  {} marked", self.marked.len()),
                fg(p.success).add_modifier(Modifier::BOLD),
            ));
        }
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
        let left = match self.bar() {
            Bar::Keys(keys) => Line::from(key_bar(keys, left_area.width, p)),
            Bar::Message(message) => Line::styled(format!(" {message}"), fg(p.warning)),
            Bar::Error(err) => Line::styled(format!(" {err}"), fg(p.danger)),
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
    /// A line across, under the fields.
    Rule,
    Image {
        target: usize,
        url: String,
        alt: String,
    },
}

impl Part {
    fn target(&self) -> Option<usize> {
        match self {
            Part::Text(_) | Part::Rule => None,
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

/// The form in `inner`, inside the border its caller draws, its own on a
/// screen of its own or [1]'s on the panels: the title, the fields the
/// pickers set, and the description below, the field the cursor is in
/// marked. `has_cursor` puts the terminal's cursor in it, unless a picker
/// is over it.
fn draw_form(frame: &mut Frame, inner: Rect, form: &Form, p: &Palette, has_cursor: bool) {
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

/// The issue's fields a row each, for [1] on the panels, each said even
/// when it is not set: status, priority and assignee, labels, project and
/// cycle, and who works on it.
fn field_rows(issue: &Issue, host: &dyn Host, p: &Palette) -> Vec<Line<'static>> {
    let label = |text: &str| Span::styled(format!("{text:<10}"), fg(p.muted));
    let dot = || Span::styled("  ·  ", fg(p.border));
    let or_none = |value: String, style: Style, none: &str| {
        if value.is_empty() {
            Span::styled(none.to_string(), fg(p.muted))
        } else {
            Span::styled(value, style)
        }
    };
    let priority = Span::styled(
        match issue.priority {
            1..=4 => issue.priority_label(),
            _ => "no priority",
        },
        priority_style(issue, p),
    );
    vec![
        Line::from(vec![
            label("status"),
            Span::styled(issue.state.clone(), fg(p.info)),
            dot(),
            priority,
            dot(),
            or_none(
                issue.assignee.clone().unwrap_or_default(),
                fg(p.text),
                "unassigned",
            ),
        ]),
        Line::from(vec![
            label("labels"),
            or_none(issue.labels.join(", "), fg(p.special), "none"),
        ]),
        Line::from(vec![
            label("project"),
            or_none(
                issue.project.clone().unwrap_or_default(),
                fg(p.text),
                "none",
            ),
            dot(),
            or_none(
                issue.cycle.clone().unwrap_or_default(),
                fg(p.muted),
                "no cycle",
            ),
        ]),
        Line::from(vec![label("work"), work_said(host, &issue.id, p)]),
    ]
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
pub fn draw_picker(frame: &mut Frame, area: Rect, picker: &Picker, p: &Palette) {
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
            _ => !picker.is_mixed && choice.id == picker.current,
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

/// The comment box in the middle of `area`, on a screen of its own.
fn draw_compose(frame: &mut Frame, area: Rect, compose: &Compose, p: &Palette) {
    let width = area.width.saturating_sub(8).min(100);
    let height = compose_height(compose, width).min(area.height);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup);
    draw_compose_box(frame, popup, compose, p, true);
}

/// The comment's rows wrapped inside a box `width` wide, between its
/// borders and a column of air each side, an empty line kept as a row.
fn compose_rows(compose: &Compose, width: u16) -> Vec<String> {
    let inner_width = usize::from(width.saturating_sub(4)).max(1);
    let mut rows: Vec<String> = Vec::new();
    for line in compose.text.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            rows.push(String::new());
        }
        rows.extend(chars.chunks(inner_width).map(|c| c.iter().collect()));
    }
    rows
}

/// How tall the comment box `width` wide is: as tall as the comment, from
/// six rows to fourteen, and its border.
fn compose_height(compose: &Compose, width: u16) -> u16 {
    let shown = compose_rows(compose, width).len().clamp(6, 14);
    u16::try_from(shown + 2).unwrap_or(u16::MAX)
}

/// The comment box in `area`: the text, wrapped, the end of a long one kept
/// in view. `has_keys` says its keys along its bottom, where no bar under
/// it says them.
fn draw_compose_box(frame: &mut Frame, area: Rect, compose: &Compose, p: &Palette, has_keys: bool) {
    let rows = compose_rows(compose, area.width);
    let mut block = Block::bordered()
        .border_style(fg(p.accent))
        .padding(Padding::horizontal(1))
        .title(Span::styled(
            format!(" Comment on {} ", compose.issue),
            fg(p.title).add_modifier(Modifier::BOLD),
        ));
    if has_keys {
        block = block.title_bottom(Line::styled(
            " Enter sends  Alt+Enter new line  Esc cancels ",
            fg(p.muted),
        ));
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
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

/// The keys, over the middle of `area`, on a screen of its own: on the
/// panels the program lists them with its own.
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
