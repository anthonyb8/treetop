//! A tree's PR in a popup over the pool: an Overview, its checks with the
//! end of a failed one's log, its review threads on the code they are about,
//! and CodeRabbit's findings sorted by kind. Any one item, or all of
//! CodeRabbit's open findings, goes to the tree's agent as a message. The
//! PR is read by the repository's ship plugin, each time the popup opens.

use std::path::PathBuf;

use crate::coderabbit::{Finding, Kind};
use crate::ship::{self, CheckView, Outcome, PrView, Thread};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Checks,
    Comments,
    CodeRabbit,
}

impl Tab {
    pub fn index(self) -> usize {
        self as usize
    }
}

/// The tabs `pr` has: CodeRabbit's only where it reviews the repository.
pub fn tabs(pr: &PrView) -> Vec<Tab> {
    let mut tabs = vec![Tab::Overview, Tab::Checks, Tab::Comments];
    if pr.bot.is_some() {
        tabs.push(Tab::CodeRabbit);
    }
    tabs
}

/// The popup: whose PR, the tab on show, and the item selected in each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrPopup {
    pub path: PathBuf,
    pub tab: Tab,
    /// The selected row of each tab, by `Tab::index`; on Overview, how far
    /// it is scrolled.
    pub selected: [usize; 4],
    /// CodeRabbit's nitpicks are listed, rather than folded into one row.
    pub nitpicks: bool,
    /// Rows scrolled past the selected item's start, for one taller than
    /// the popup; moving the selection starts it again at 0.
    pub offset: usize,
}

impl PrPopup {
    pub fn new(path: PathBuf) -> Self {
        PrPopup {
            path,
            tab: Tab::Overview,
            selected: [0; 4],
            nitpicks: false,
            offset: 0,
        }
    }

    pub fn selected(&self) -> usize {
        self.selected[self.tab.index()]
    }
}

/// A tree's PR as last read, and whether a read is under way: the last one
/// shows while the next comes in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrRead {
    /// None until the first read lands; inside, None for a branch with no PR.
    pub pr: Option<Result<Option<PrView>, String>>,
    pub is_reading: bool,
}

/// CodeRabbit's findings as listed: nitpicks only once shown.
pub fn findings(pr: &PrView, nitpicks: bool) -> Vec<&Finding> {
    pr.bot
        .iter()
        .flat_map(|bot| &bot.findings)
        .filter(|f| nitpicks || f.kind != Kind::Nitpick)
        .collect()
}

/// CodeRabbit's open findings that are not nitpicks, which `F` sends.
pub fn actionable(pr: &PrView) -> Vec<&Finding> {
    findings(pr, false)
        .into_iter()
        .filter(|f| !f.resolved)
        .collect()
}

pub fn nitpick_count(pr: &PrView) -> usize {
    pr.bot
        .iter()
        .flat_map(|bot| &bot.findings)
        .filter(|f| f.kind == Kind::Nitpick)
        .count()
}

/// The last row the selection can reach on `tab`. The Overview scrolls,
/// and stops about where it ends: its summary, then the description.
pub fn last_row(pr: &PrView, tab: Tab, nitpicks: bool) -> usize {
    let rows = match tab {
        Tab::Overview => return pr.body.lines().count() + 12,
        Tab::Checks => pr.checks.len(),
        Tab::Comments => pr.threads.len(),
        Tab::CodeRabbit => findings(pr, nitpicks).len(),
    };
    rows.saturating_sub(1)
}

/// Where a finding or thread is: `src/a.rs:12-14`, or the file alone.
pub fn place(path: Option<&str>, lines: Option<&str>) -> String {
    match (path, lines) {
        (Some(path), Some(lines)) => format!("{path}:{lines}"),
        (Some(path), None) => path.to_string(),
        (None, _) => String::new(),
    }
}

fn capped(text: String) -> String {
    ship::cap(&text, ship::MESSAGE_CAP).to_string()
}

/// What `f` sends for a failed check: what it is, then its log's end.
pub fn check_message(pr: &PrView, check: &CheckView) -> Option<String> {
    (check.outcome == Outcome::Failed).then(|| {
        let log = check.log.as_deref().unwrap_or("Its log could not be read.");
        capped(format!(
            "{} failed on #{}. Fix it and push.\n\n{}",
            check.name,
            pr.number,
            log.trim()
        ))
    })
}

/// What `f` sends for a review thread: who said it where, the code it is
/// on, then what was said.
pub fn thread_message(pr: &PrView, thread: &Thread) -> String {
    let at = place(
        thread.path.as_deref(),
        thread.line.map(|l| l.to_string()).as_deref(),
    );
    let mut text = if at.is_empty() {
        format!(
            "{} commented on #{}. Address it and push.",
            thread.author, pr.number
        )
    } else {
        format!(
            "{} commented on {at} in #{}. Address it and push.",
            thread.author, pr.number
        )
    };
    if !thread.code.is_empty() {
        text.push_str(&format!("\n\n```diff\n{}\n```", thread.code.join("\n")));
    }
    text.push_str(&format!("\n\n{}", thread.body));
    for reply in &thread.replies {
        text.push_str(&format!("\n\n{} replied: {}", reply.author, reply.body));
    }
    capped(text)
}

/// One finding as an agent gets it: CodeRabbit's own prompt for agents where
/// it wrote one, else its text and suggested change.
fn finding_text(finding: &Finding) -> String {
    let at = place(finding.path.as_deref(), finding.lines.as_deref());
    let mut text = format!("{at}: {}", finding.title);
    match &finding.prompt {
        Some(prompt) => text.push_str(&format!("\n\n{prompt}")),
        None => {
            if !finding.body.is_empty() {
                text.push_str(&format!("\n\n{}", finding.body));
            }
            if !finding.fix.is_empty() {
                text.push_str(&format!(
                    "\n\nSuggested change:\n```diff\n{}\n```",
                    finding.fix.join("\n")
                ));
            }
        }
    }
    text
}

/// What `f` sends for one of CodeRabbit's findings.
pub fn finding_message(pr: &PrView, finding: &Finding) -> String {
    capped(format!(
        "CodeRabbit flagged this on #{}. Fix it if it still holds, and push.\n\n{}",
        pr.number,
        finding_text(finding)
    ))
}

/// What `F` sends: every open finding but the nitpicks.
pub fn findings_message(pr: &PrView, findings: &[&Finding]) -> String {
    let each: Vec<String> = findings.iter().map(|f| finding_text(f)).collect();
    let count = match findings.len() {
        1 => "1 open comment".to_string(),
        n => format!("{n} open comments"),
    };
    capped(format!(
        "CodeRabbit left {count} on #{}. Fix the ones that still hold, and push.\n\n{}",
        pr.number,
        each.join("\n\n---\n\n")
    ))
}
