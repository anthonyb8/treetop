//! What a planning chat made: the Linear issues it created, found in its
//! transcript where `keel linear create` says so, as
//! `<ID> <url> (team T, workspace W)`, and where each stands in Linear.
//! Reading the transcript catches every issue the chat files, by `S` or a
//! `/spec` typed into it, with nothing for the chat to remember to do.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use issues::Backend;
use serde_json::Value;

/// The ids `keel linear create` says it made in `text`, its output, in
/// order: a line `<ID> <url> (team T, workspace W)`.
pub fn created(text: &str) -> Vec<String> {
    const URL: &str = " https://linear.app/";
    let mut ids = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(URL).map(|at| from + at) {
        from = at + URL.len();
        let rest = &text[from..];
        let Some(end) = rest.find(' ') else {
            continue;
        };
        if !rest[end..].starts_with(" (team ") {
            continue;
        }
        ids.extend(id_before(&text[..at]));
    }
    ids
}

/// The issue id that ends `text`, such as `ANB-12` in `...\nANB-12`: an
/// upper-case team key, a dash and a number.
fn id_before(text: &str) -> Option<String> {
    let start = text
        .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .map_or(0, |at| at + 1);
    let id = &text[start..];
    let (team, number) = id.split_once('-')?;
    let is_team = team
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    let is_number = !number.is_empty() && number.chars().all(|c| c.is_ascii_digit());
    (is_team && is_number).then(|| id.to_string())
}

/// Reads each transcript on from where it last stopped, so a long chat is
/// read once, not again every look.
#[derive(Default)]
pub struct Reader {
    read: HashMap<PathBuf, TranscriptRead>,
}

/// How far a transcript has been read, and its tool calls so far that run
/// `keel linear create`, whose results say what they made.
#[derive(Default)]
struct TranscriptRead {
    to: u64,
    creates: HashSet<String>,
}

impl Reader {
    /// The issues the transcript at `path` shows `keel linear create`
    /// making since the last read of it, up to its last whole line: only
    /// the output of a call running it, so a create line the chat read in
    /// a file or another command's output does not count. A transcript
    /// shorter than where it stopped was written again, and is read from
    /// its start.
    pub fn new_ids(&mut self, path: &PathBuf) -> Vec<String> {
        let Ok(mut file) = File::open(path) else {
            return Vec::new();
        };
        let len = file.metadata().map_or(0, |m| m.len());
        let read = self.read.entry(path.clone()).or_default();
        if len < read.to {
            *read = TranscriptRead::default();
        }
        if file.seek(SeekFrom::Start(read.to)).is_err() {
            return Vec::new();
        }
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            return Vec::new();
        }
        let whole = bytes
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |at| at + 1);
        read.to += u64::try_from(whole).unwrap_or(0);
        let mut ids: Vec<String> = Vec::new();
        for line in String::from_utf8_lossy(&bytes[..whole]).lines() {
            let Ok(entry) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let items = entry
                .pointer("/message/content")
                .and_then(Value::as_array)
                .map_or(&[][..], Vec::as_slice);
            for item in items {
                let is_create = item["input"]["command"]
                    .as_str()
                    .is_some_and(|c| c.contains("keel linear create"));
                let answers_create = item["tool_use_id"]
                    .as_str()
                    .is_some_and(|id| read.creates.contains(id));
                match item["type"].as_str() {
                    Some("tool_use") if is_create => {
                        if let Some(id) = item["id"].as_str() {
                            read.creates.insert(id.to_string());
                        }
                    }
                    Some("tool_result") if answers_create => {
                        ids.extend(created(&output(&item["content"])));
                    }
                    _ => {}
                }
            }
        }
        let mut seen = HashSet::new();
        ids.retain(|id| seen.insert(id.clone()));
        ids
    }
}

/// A tool result's text: a string, or the text of its parts.
fn output(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Where a planned issue stands, as its status's kind in Linear says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    NotStarted,
    Going,
    Done,
    Canceled,
}

/// A planning chat's issue as Linear has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub id: String,
    pub title: String,
    pub project: Option<String>,
    pub state: String,
    pub stage: Stage,
    /// The issues blocking it that are not done yet.
    pub blocked_by: Vec<String>,
}

/// Each of `ids` as `backend` has it, with its stage by its team's states;
/// an issue it cannot read is left out.
pub fn read(backend: &dyn Backend, ids: &[String]) -> Vec<Planned> {
    let team = backend.team().ok().flatten();
    let kind_of = |state_id: Option<&str>| {
        let kind = team
            .as_ref()
            .and_then(|t| t.states.iter().find(|s| Some(s.id.as_str()) == state_id))
            .map(|s| s.kind.as_str());
        match kind {
            Some("started") => Stage::Going,
            Some("completed") => Stage::Done,
            Some("canceled") => Stage::Canceled,
            _ => Stage::NotStarted,
        }
    };
    ids.iter()
        .filter_map(|id| backend.detail(id).ok())
        .map(|detail| Planned {
            stage: kind_of(detail.issue.state_id.as_deref()),
            blocked_by: detail
                .links
                .iter()
                .filter(|l| l.kind == "blocked by" && !l.is_done)
                .map(|l| l.id.clone())
                .collect(),
            id: detail.issue.id,
            title: detail.issue.title,
            project: detail.issue.project,
            state: detail.issue.state,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A transcript's lines: a Bash call and its result.
    fn call(id: &str, command: &str, output: &str) -> String {
        let used = serde_json::json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}}
        ]}});
        let result = serde_json::json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": id, "content": output}
        ]}});
        format!("{used}\n{result}\n")
    }

    fn scratch(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("treetop-planned-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// A Linear with ANB-1 started, ANB-2 done, ANB-3 canceled and ANB-4 in
    /// Backlog, blocked by ANB-2 (done) and ANB-1 (not), and ANB-5 it cannot
    /// read.
    struct Fake;

    impl Backend for Fake {
        fn name(&self) -> &str {
            "work"
        }

        fn list(&self, _: &issues::Filters) -> Result<Vec<linear::Issue>, String> {
            Ok(Vec::new())
        }

        fn detail(&self, id: &str) -> Result<linear::Detail, String> {
            let state = match id {
                "ANB-1" => "Doing",
                "ANB-2" => "Done",
                "ANB-3" => "Dropped",
                "ANB-4" => "Backlog",
                _ => return Err(format!("no {id}")),
            };
            let link = |id: &str, is_done| linear::Link {
                kind: "blocked by".into(),
                id: id.into(),
                title: String::new(),
                state: String::new(),
                is_done,
            };
            Ok(linear::Detail {
                issue: linear::Issue {
                    id: id.into(),
                    title: format!("{id} title"),
                    state: state.into(),
                    state_id: Some(state.into()),
                    project: Some("treetop panels".into()),
                    ..linear::Issue::default()
                },
                comments: Vec::new(),
                links: if id == "ANB-4" {
                    vec![link("ANB-2", true), link("ANB-1", false)]
                } else {
                    Vec::new()
                },
            })
        }

        fn team(&self) -> Result<Option<std::sync::Arc<linear::Team>>, String> {
            let state = |name: &str, kind: &str| linear::State {
                id: name.into(),
                name: name.into(),
                kind: kind.into(),
                position: 0.0,
            };
            Ok(Some(std::sync::Arc::new(linear::Team {
                id: "t".into(),
                key: "ANB".into(),
                name: "ANB".into(),
                states: vec![
                    state("Backlog", "backlog"),
                    state("Doing", "started"),
                    state("Done", "completed"),
                    state("Dropped", "canceled"),
                ],
                labels: Vec::new(),
                members: Vec::new(),
                projects: Vec::new(),
                cycles: Vec::new(),
                me: None,
            })))
        }
    }

    #[test]
    fn each_issue_stands_where_its_status_kind_says_and_waits_only_on_blockers_not_done() {
        let ids: Vec<String> = (1..=5).map(|n| format!("ANB-{n}")).collect();
        let read = read(&Fake, &ids);
        let stages: Vec<(&str, Stage)> = read.iter().map(|p| (p.id.as_str(), p.stage)).collect();
        assert_eq!(
            stages,
            [
                ("ANB-1", Stage::Going),
                ("ANB-2", Stage::Done),
                ("ANB-3", Stage::Canceled),
                ("ANB-4", Stage::NotStarted),
            ],
            "ANB-5, unreadable, is left out"
        );
        assert_eq!(read[3].blocked_by, ["ANB-1"]);
        assert_eq!(read[3].project.as_deref(), Some("treetop panels"));
    }

    const MADE: &str = "ANB-101 https://linear.app/a/issue/ANB-101/r (team ANB, workspace anb)";

    #[test]
    fn only_what_keel_linear_create_said_counts_not_a_create_line_quoted() {
        let path = scratch("quoted");
        let mut text = call("t1", "cat src/planned.rs", MADE);
        text += &call("t2", "keel linear get ANB-9", MADE);
        text += &call(
            "t3",
            "cd /code/here && keel linear create --title x --body f",
            &MADE.replace("101", "205"),
        );
        std::fs::write(&path, text).unwrap();
        assert_eq!(Reader::default().new_ids(&path), ["ANB-205"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_transcript_read_on_from_where_it_stopped_and_again_once_rewritten() {
        let path = scratch("reread");
        let create = |n: &str| {
            call(
                &format!("t{n}"),
                "keel linear create",
                &MADE.replace("101", n),
            )
        };
        let mut reader = Reader::default();
        std::fs::write(&path, create("1")).unwrap();
        assert_eq!(reader.new_ids(&path), ["ANB-1"]);
        std::fs::write(&path, create("1") + &create("2")).unwrap();
        assert_eq!(reader.new_ids(&path), ["ANB-2"], "only what is new");
        // Written again, shorter: read from its start.
        std::fs::write(&path, create("3")).unwrap();
        assert_eq!(reader.new_ids(&path), ["ANB-3"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn each_issue_a_create_lists_counts_once_and_its_project_and_other_links_do_not() {
        let path = scratch("lines");
        let output = [
            "ANB-101 https://linear.app/anbgroup/issue/ANB-101/read-every (team ANB, workspace anb)",
            "ANB-102 https://linear.app/anbgroup/issue/ANB-102/add-a-panels (team ANB, workspace anb)",
            "treetop panels https://linear.app/anbgroup/project/treetop-panels-7199",
            "ANB-7 https://linear.app/anbgroup/issue/ANB-7 was mentioned",
            "ANB-101 https://linear.app/anbgroup/issue/ANB-101/read-every (team ANB, workspace anb)",
        ]
        .join("\n");
        std::fs::write(&path, call("t1", "keel linear create --project p", &output)).unwrap();
        assert_eq!(Reader::default().new_ids(&path), ["ANB-101", "ANB-102"]);
        let _ = std::fs::remove_file(&path);
    }
}
