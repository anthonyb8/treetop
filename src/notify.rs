//! Desktop notifications on the machine treetop's terminal runs on, such as
//! a laptop reaching the server over SSH: the terminal's own escape for one,
//! OSC 777, which travels with treetop's output and, wrapped, through tmux,
//! so nothing more runs on the server. The terminal shows it as the desktop
//! does, with its sound, beside the bell treetop rings.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::inbox::{About, Entry, Kind, place_of};

/// How long something waits before the desktop is told again, once.
const AGAIN: Duration = Duration::from_secs(30 * 60);

/// How soon after the first the reminder may come, however late the first.
const GAP: Duration = Duration::from_secs(25 * 60);

/// How long something can drop out of what waits, as an agent read as busy
/// for a moment, and come back as the same thing, not told again.
const FORGET: Duration = Duration::from_secs(2 * 60);

/// What the desktop has been told about one thing waiting.
#[derive(Debug, Clone, Copy)]
struct Told {
    is_reminded: bool,
    /// When it was last told, for GAP.
    at: SystemTime,
    /// When it was last seen waiting, for FORGET.
    seen: SystemTime,
}

/// What the desktop has been told, and whether treetop has said that tmux
/// keeps its notifications from the terminal.
#[derive(Debug, Default)]
pub struct Desktop {
    /// None until the first look, which tells nothing, so what already
    /// waits when treetop opens is not announced again.
    told: Option<HashMap<About, Told>>,
    /// The other workspaces read so far: what waits in one at its first
    /// read was already waiting, and is not announced either.
    read: HashSet<PathBuf>,
    /// When it last looked, so something not seen since before then has
    /// been out of what waits.
    looked: Option<SystemTime>,
    /// treetop has switched workspace since the last look: what waits in
    /// the new one is already waiting, so it is not announced, while what
    /// was told before keeps its reminder.
    is_switched: bool,
    is_passthrough_said: bool,
}

impl Desktop {
    /// What to tell the desktop of as of `now`, from what is `waiting`, with
    /// the other workspaces `read` so far: each thing that has started
    /// waiting since the last look, and, once, each that has waited AGAIN,
    /// no sooner than GAP after it was first told. The flag is set on a
    /// reminder.
    pub fn due(
        &mut self,
        waiting: &[Entry],
        read: &[PathBuf],
        now: SystemTime,
    ) -> Vec<(Entry, bool)> {
        let known = |now| Told {
            is_reminded: true,
            at: now,
            seen: now,
        };
        let first_read: HashSet<&PathBuf> =
            read.iter().filter(|r| !self.read.contains(*r)).collect();
        self.read.extend(read.iter().cloned());
        let looked = self.looked.replace(now);
        let Some(told) = self.told.as_mut() else {
            self.told = Some(waiting.iter().map(|e| (e.about(), known(now))).collect());
            return Vec::new();
        };
        let since = |at: SystemTime| now.duration_since(at).unwrap_or_default();
        // Out of what waits since before the last look, for FORGET or more:
        // forgotten, so it is new if it comes back.
        let is_gone = |t: &Told| looked.is_some_and(|l| t.seen < l) && since(t.seen) >= FORGET;
        told.retain(|_, t| !is_gone(t));
        let is_switched = std::mem::take(&mut self.is_switched);
        let mut due = Vec::new();
        for entry in waiting {
            let is_first_read = entry
                .away
                .as_ref()
                .is_some_and(|a| first_read.contains(&a.checkout));
            match told.get_mut(&entry.about()) {
                None if is_first_read || is_switched => {
                    told.insert(entry.about(), known(now));
                }
                None => {
                    told.insert(
                        entry.about(),
                        Told {
                            is_reminded: false,
                            at: now,
                            seen: now,
                        },
                    );
                    due.push((entry.clone(), false));
                }
                Some(t) => {
                    t.seen = now;
                    let is_again = entry.at.is_some_and(|waited| since(waited) >= AGAIN);
                    if !t.is_reminded && is_again && since(t.at) >= GAP {
                        t.is_reminded = true;
                        t.at = now;
                        due.push((entry.clone(), true));
                    }
                }
            }
        }
        due
    }

    /// treetop is showing another workspace now.
    pub fn switched(&mut self) {
        self.is_switched = true;
    }

    /// The bytes that show each of `due` as of `now`, and what to say on
    /// the status line. Inside tmux, whose `allow-passthrough` is
    /// `passthrough`, with it off nothing is shown, and treetop says so
    /// once and leaves the setting to you; outside tmux, `passthrough` is
    /// None.
    pub fn tell(
        &mut self,
        due: &[(Entry, bool)],
        passthrough: Option<&str>,
        now: SystemTime,
    ) -> (Vec<u8>, Option<String>) {
        if passthrough.is_some_and(|p| !is_passed(p)) {
            let said = (!self.is_passthrough_said && !due.is_empty()).then(|| {
                "desktop notifications need tmux to pass them on from any window: tmux set -g allow-passthrough all"
                    .to_string()
            });
            self.is_passthrough_said |= said.is_some();
            return (Vec::new(), said);
        }
        let bytes = due
            .iter()
            .flat_map(|(entry, is_reminder)| {
                let (title, body) = text(entry, *is_reminder, now);
                escape(&title, &body, passthrough.is_some())
            })
            .collect();
        (bytes, None)
    }
}

/// The notification's title and body: the tree, with its workspace when it
/// is another, its ticket and branch, what it waits for, and the question
/// for an ask, since this one stays on your own screen.
pub fn text(entry: &Entry, is_reminder: bool, now: SystemTime) -> (String, String) {
    let title = match &entry.title {
        Some(title) => format!("{} · {title}", place_of(entry)),
        None => place_of(entry),
    };
    let what = crate::ui::waits_for(entry.kind);
    let mut body = if is_reminder {
        let waited = entry
            .at
            .and_then(|at| now.duration_since(at).ok())
            .unwrap_or_default();
        format!("Still {what}, {}m", waited.as_secs() / 60)
    } else {
        let mut what = what.to_string();
        what[..1].make_ascii_uppercase();
        what
    };
    if entry.kind == Kind::NeedsYou && !entry.what.is_empty() {
        body = format!("{body}: {}", entry.what);
    }
    (title, body)
}

/// The bytes that show a notification of `title` and `body`: OSC 777,
/// wrapped for tmux to pass on when `is_in_tmux`. A `;` in the title would
/// end it early, and control characters would end the escape, so neither
/// goes through.
pub fn escape(title: &str, body: &str, is_in_tmux: bool) -> Vec<u8> {
    let clean = |text: &str| -> String { text.chars().filter(|c| !c.is_control()).collect() };
    let osc = format!(
        "\x1b]777;notify;{};{}\x1b\\",
        clean(title).replace(';', ","),
        clean(body)
    );
    if !is_in_tmux {
        return osc.into_bytes();
    }
    // tmux passes on what is inside its own DCS, each ESC doubled.
    format!("\x1bPtmux;{}\x1b\\", osc.replace('\x1b', "\x1b\x1b")).into_bytes()
}

/// tmux's `allow-passthrough`, as tmux gives it, lets an escape through to
/// the terminal whichever window is on screen. `on` passes it only from the
/// window on screen, which is when the count says it already, not while
/// you are in another window or away.
fn is_passed(allow_passthrough: &str) -> bool {
    allow_passthrough.trim() == "all"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(tree: &str, kind: Kind, at: Option<SystemTime>) -> Entry {
        Entry {
            kind,
            away: None,
            tree: tree.into(),
            path: PathBuf::from(format!("/pool/{tree}")),
            title: Some("ANB-73 compose-history".into()),
            what: "Keep drafts per tree, or one for all?".into(),
            at,
        }
    }

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn what_starts_waiting_is_told_at_once_and_again_at_thirty_minutes() {
        let start = SystemTime::now();
        let at = |m: u32| start + MIN * m;
        let mut desktop = Desktop::default();
        let already = entry("9", Kind::Died, Some(start));
        assert!(
            desktop
                .due(std::slice::from_ref(&already), &[], at(0))
                .is_empty(),
            "not at the first look"
        );
        let asks = entry("22", Kind::NeedsYou, Some(at(1)));
        let both = [already.clone(), asks.clone()];
        let told = desktop.due(&both, &[], at(1));
        assert_eq!(told.len(), 1);
        assert_eq!((told[0].0.tree.as_str(), told[0].1), ("22", false));
        assert!(desktop.due(&both, &[], at(2)).is_empty(), "once");
        assert!(
            desktop.due(&both, &[], at(30)).is_empty(),
            "29 minutes is not 30"
        );
        let again = desktop.due(&both, &[], at(31));
        assert_eq!(again.len(), 1);
        assert_eq!((again[0].0.tree.as_str(), again[0].1), ("22", true));
        assert!(desktop.due(&both, &[], at(120)).is_empty(), "then never");
        // Answered, then asking again later, it is new.
        desktop.due(std::slice::from_ref(&already), &[], at(121));
        assert_eq!(desktop.due(&both, &[], at(124)).len(), 1);
    }

    #[test]
    fn the_reminder_waits_its_gap_after_a_first_told_late() {
        let start = SystemTime::now();
        let at = |m: u32| start + MIN * m;
        let mut desktop = Desktop::default();
        desktop.due(&[], &[], at(0));
        // Seen first at minute 40, though it has waited since minute 0.
        let old = [entry("7", Kind::Failed, Some(start))];
        assert_eq!(desktop.due(&old, &[], at(40)).len(), 1);
        assert!(desktop.due(&old, &[], at(41)).is_empty());
        assert_eq!(desktop.due(&old, &[], at(65)).len(), 1);
    }

    #[test]
    fn an_ask_that_flickers_out_and_back_is_told_once() {
        let start = SystemTime::now();
        let at = |m: u32| start + MIN * m;
        let mut desktop = Desktop::default();
        desktop.due(&[], &[], at(0));
        let asks = [entry("22", Kind::NeedsYou, Some(at(1)))];
        assert_eq!(desktop.due(&asks, &[], at(1)).len(), 1);
        let moment = at(1) + Duration::from_secs(30);
        assert!(
            desktop.due(&[], &[], moment).is_empty(),
            "busy for a moment"
        );
        assert!(desktop.due(&asks, &[], at(2)).is_empty(), "the same ask");
        // Gone for longer than that, it is a new one.
        desktop.due(&[], &[], at(4));
        assert_eq!(desktop.due(&asks, &[], at(10)).len(), 1);
    }

    #[test]
    fn what_waits_in_another_workspace_at_its_first_read_is_not_news() {
        let start = SystemTime::now();
        let at = |m: u32| start + MIN * m;
        let there = PathBuf::from("/code/endodeals-web");
        let away = |tree: &str| Entry {
            away: Some(crate::inbox::Away {
                name: "endodeals-web".into(),
                checkout: there.clone(),
            }),
            ..entry(tree, Kind::Failed, None)
        };
        let mut desktop = Desktop::default();
        desktop.due(&[], &[], at(0));
        // Its first read, a few seconds on, finds a PR failing already.
        assert!(
            desktop
                .due(&[away("7")], std::slice::from_ref(&there), at(1))
                .is_empty()
        );
        // Another failing at its next read is news.
        let both = [away("7"), away("5")];
        let told = desktop.due(&both, std::slice::from_ref(&there), at(4));
        assert_eq!(told.len(), 1);
        assert_eq!(told[0].0.tree, "5");
    }

    #[test]
    fn with_tmux_keeping_it_back_nothing_shows_and_treetop_says_so_once() {
        let now = SystemTime::now();
        let due = [(entry("22", Kind::NeedsYou, Some(now)), false)];
        let mut desktop = Desktop::default();
        // `on` passes it only while treetop's window is on screen, which is
        // when the count says it already.
        let (bytes, said) = desktop.tell(&due, Some("on"), now);
        assert!(bytes.is_empty());
        assert!(said.unwrap().contains("allow-passthrough all"));
        assert_eq!(
            desktop.tell(&due, Some("off"), now),
            (Vec::new(), None),
            "once"
        );
        let (bytes, said) = desktop.tell(&due, Some("all"), now);
        assert!(
            bytes.starts_with(b"\x1bPtmux;") && said.is_none(),
            "through tmux"
        );
        let (bytes, _) = desktop.tell(&due, None, now);
        assert!(
            bytes.starts_with(b"\x1b]777;notify;22 "),
            "outside tmux, as it is"
        );
    }

    #[test]
    fn a_notification_names_the_tree_and_an_ask_its_question() {
        let now = SystemTime::now();
        let asks = entry("22", Kind::NeedsYou, Some(now - 31 * MIN));
        assert_eq!(
            text(&asks, false, now),
            (
                "22 · ANB-73 compose-history".into(),
                "Needs you: Keep drafts per tree, or one for all?".into()
            )
        );
        assert_eq!(
            text(&asks, true, now).1,
            "Still needs you, 31m: Keep drafts per tree, or one for all?"
        );
        // Failed checks and a dead agent are told the same way, without
        // the question, which is only an ask's.
        let failed = entry("7", Kind::Failed, None);
        assert_eq!(text(&failed, false, now).1, "Checks failed");
        let died = entry("9", Kind::Died, None);
        assert_eq!(text(&died, false, now).1, "Agent died");
    }

    #[test]
    fn the_escape_reaches_the_terminal_through_tmux_and_cannot_be_cut_short() {
        assert_eq!(
            escape("22 · a;b", "Needs you\x07: x", false),
            b"\x1b]777;notify;22 \xc2\xb7 a,b;Needs you: x\x1b\\".to_vec()
        );
        assert_eq!(
            escape("t", "b", true),
            b"\x1bPtmux;\x1b\x1b]777;notify;t;b\x1b\x1b\\\x1b\\".to_vec()
        );
    }

    #[test]
    fn tmux_passes_the_escape_on_from_a_hidden_window_only_with_passthrough_all() {
        assert!(is_passed("all\n"));
        assert!(!is_passed("on"), "only while the window is on screen");
        assert!(!is_passed("off") && !is_passed(""), "off, or tmux silent");
    }

    #[test]
    fn a_switch_of_workspace_keeps_the_reminder_and_announces_nothing() {
        let start = SystemTime::now();
        let at = |m: u32| start + MIN * m;
        let mut desktop = Desktop::default();
        desktop.due(&[], &[], at(0));
        let asks = entry("22", Kind::NeedsYou, Some(at(1)));
        assert_eq!(
            desktop.due(std::slice::from_ref(&asks), &[], at(1)).len(),
            1
        );
        // The next workspace, where tree 30's checks were failing already.
        desktop.switched();
        let there = [asks.clone(), entry("30", Kind::Failed, None)];
        assert!(desktop.due(&there, &[], at(2)).is_empty(), "nothing new");
        let again = desktop.due(&there, &[], at(31));
        assert_eq!(again.len(), 1);
        assert_eq!((again[0].0.tree.as_str(), again[0].1), ("22", true));
    }
}
