//! CodeRabbit's review comments read into findings: what kind each is, how
//! severe, its title and text, any change it suggests, and the prompt it
//! writes for an agent. CodeRabbit's comments are Markdown and HTML for a
//! browser rather than an API, so this reads the markers it leaves in them
//! (`cr-indicator-types`, `ai-agent-prompt`, `cr-comment`) where they are,
//! and its older headings (`_⚠️ Potential issue_`) where they are not; a
//! comment it cannot read still comes through whole, as a plain comment.

use serde::{Deserialize, Serialize};

/// In the order the CodeRabbit tab lists them: issues first, nitpicks last.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Issue,
    Suggestion,
    #[default]
    Comment,
    Nitpick,
}

/// One thing CodeRabbit asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub kind: Kind,
    /// `Critical`, `Major`, `Minor` or `Trivial`, as CodeRabbit rates it.
    #[serde(default)]
    pub severity: Option<String>,
    /// The file it is about; a line comment's comes from its thread.
    #[serde(default)]
    pub path: Option<String>,
    /// `183-188` or `148`.
    #[serde(default)]
    pub lines: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// The change it suggests, as diff lines starting `+`, `-` or a space.
    #[serde(default)]
    pub fix: Vec<String>,
    /// What it wrote for an agent to act on, which is what `f` sends.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Marked addressed by a later commit, or its thread resolved.
    #[serde(default)]
    pub resolved: bool,
}

const PROMPT_START: &str = "<!-- ai-agent-prompt:start -->";
const PROMPT_END: &str = "<!-- ai-agent-prompt:end -->";
const COMMENT_END: &str = "<!-- cr-comment:v1:";
const INDICATOR: &str = "<!-- cr-indicator-types:";

/// Whether `login` is CodeRabbit's: `coderabbitai` in GraphQL,
/// `coderabbitai[bot]` in REST.
pub fn is_coderabbit(login: &str) -> bool {
    login == "coderabbitai" || login == "coderabbitai[bot]"
}

/// The findings in one of CodeRabbit's line comments, which holds one or a
/// few, each closed by its `cr-comment` marker.
pub fn line_comment(body: &str) -> Vec<Finding> {
    let addressed = body.contains("Addressed in commit");
    let mut findings: Vec<Finding> = chunks(body)
        .into_iter()
        .map(|chunk| finding(chunk, Kind::Comment))
        .filter(|f| !f.title.is_empty() || !f.body.is_empty())
        .collect();
    for finding in &mut findings {
        finding.resolved |= addressed;
    }
    findings
}

/// The findings a CodeRabbit review's own text carries, outside its line
/// comments: its nitpicks, and comments on lines outside the diff, each
/// under its file's heading.
pub fn review(body: &str) -> Vec<Finding> {
    let body = without_pictures(body);
    let mut findings = Vec::new();
    let mut section: Option<Kind> = None;
    let mut path: Option<String> = None;
    let mut item: Option<(String, String)> = None;
    let mut close = |item: &mut Option<(String, String)>, path: &Option<String>, kind: Kind| {
        if let Some((lines, text)) = item.take() {
            let mut found = finding(&text, kind);
            found.path.clone_from(path);
            found.lines = Some(lines);
            if !found.title.is_empty() || !found.body.is_empty() {
                findings.push(found);
            }
        }
    };
    for line in body.lines() {
        let trimmed = line.trim();
        // A heading opens a section or a file; any other `<summary>`, such as
        // an item's own "Prompt for AI Agents", is part of the item.
        let heading = summary(trimmed).and_then(|summary| {
            let lower = summary.to_lowercase();
            if lower.contains("nitpick comments") {
                Some((Some(Kind::Nitpick), None))
            } else if lower.contains("outside diff range") {
                Some((Some(Kind::Issue), None))
            } else if lower.contains("comments") || lower.starts_with("prompt to fix") {
                Some((None, None))
            } else {
                let file = file_of(&summary);
                (section.is_some() && file != summary).then_some((section, Some(file)))
            }
        });
        if let Some(opened) = heading {
            close(&mut item, &path, section.unwrap_or_default());
            (section, path) = opened;
            continue;
        }
        let Some(kind) = section else { continue };
        if let Some((lines, rest)) = item_start(trimmed) {
            close(&mut item, &path, kind);
            item = Some((lines, rest.to_string()));
        } else if let Some((_, text)) = item.as_mut() {
            text.push('\n');
            text.push_str(line);
            if trimmed.starts_with(COMMENT_END) {
                close(&mut item, &path, kind);
            }
        }
    }
    close(&mut item, &path, section.unwrap_or_default());
    findings
}

/// The comment cut at each `cr-comment` marker; one chunk when it has none.
fn chunks(body: &str) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find(COMMENT_END) {
        let end = rest[at..].find("-->").map_or(rest.len(), |e| at + e + 3);
        chunks.push(&rest[..end]);
        rest = &rest[end..];
    }
    if chunks.is_empty() {
        chunks.push(body);
    }
    chunks
}

/// One finding's text read: `default` is its kind when nothing in it says.
fn finding(text: &str, default: Kind) -> Finding {
    let text = without_pictures(text);
    let mut found = Finding {
        kind: indicator(&text).unwrap_or(default),
        ..Finding::default()
    };
    let (text, prompt) = take_prompt(&text);
    found.prompt = prompt;
    let (text, fix) = take_fix(&text);
    found.fix = fix;
    let mut lines = plain(&text);
    lines.retain(|l| {
        let addressed = l.contains("Addressed in commit");
        found.resolved |= addressed;
        !addressed
    });
    let mut lines = lines.into_iter().peekable();
    let skip_blanks = |lines: &mut std::iter::Peekable<std::vec::IntoIter<String>>| {
        while lines.next_if(String::is_empty).is_some() {}
    };
    skip_blanks(&mut lines);
    // One rating line or more: the older `_Potential issue_ | _Major_`
    // can come before `**Category** | **Major** | **Quick win**`.
    while let Some(line) = lines.next_if(|l| is_rating(l)) {
        if let Some(severity) = rating(&line, &mut found.kind) {
            found.severity = Some(severity);
        }
        skip_blanks(&mut lines);
    }
    // A security finding rates itself under labelled lines, such as
    // `**Reachability:** External`, before its title; they follow its text.
    let mut labels = Vec::new();
    while let Some(line) = lines.next_if(|l| l.starts_with("**") && l.contains(":**")) {
        labels.push(line.replace("**", ""));
        skip_blanks(&mut lines);
    }
    // The title is a bold line, or a bold start the text runs on from.
    let mut lead = None;
    if let Some(title) = lines.next_if(|l| l.starts_with("**")) {
        match title[2..].find("**") {
            Some(end) if !title.ends_with("**") || end + 4 < title.len() => {
                found.title = title[2..2 + end].trim().to_string();
                lead = Some(title[2 + end + 2..].trim().to_string());
            }
            _ => found.title = title.trim_matches('*').trim().to_string(),
        }
    }
    let rest: Vec<String> = lead.into_iter().chain(lines).collect();
    found.body = rest.join("\n").trim().to_string();
    if !labels.is_empty() {
        found.body = format!("{}\n\n{}", found.body, labels.join("\n"))
            .trim()
            .to_string();
    }
    if found.title.is_empty() {
        // An old or unreadable comment: its first paragraph names it.
        let (title, body) = found.body.split_once('\n').unwrap_or((&found.body, ""));
        (found.title, found.body) = (title.to_string(), body.trim().to_string());
    }
    found
}

/// What CodeRabbit's marker says the finding is.
fn indicator(text: &str) -> Option<Kind> {
    let at = text.find(INDICATOR)? + INDICATOR.len();
    let types = &text[at..text[at..].find("-->").map_or(text.len(), |e| at + e)];
    let kind = types.split(',').next()?.trim();
    Some(match kind {
        "potential_issue" => Kind::Issue,
        "refactor_suggestion" => Kind::Suggestion,
        "nitpick" => Kind::Nitpick,
        _ => Kind::Comment,
    })
}

/// The rating line that opens a finding: `**Minor** · **Correctness**` in
/// the current format, `_⚠️ Potential issue_ | _🟠 Major_` in the old.
fn is_rating(line: &str) -> bool {
    (line.starts_with("**") && (line.contains(" · ") || line.contains("** | **")))
        || (line.starts_with('_') && line.ends_with('_'))
}

/// How CodeRabbit rates a finding, worst first.
const SEVERITIES: [&str; 5] = ["Critical", "Major", "Minor", "Trivial", "Info"];

/// The severity a rating line gives, and the kind where an old-format line
/// is the only thing that says it. Its parts are split by ` · ` or `|`,
/// each in `**` or `_`, often after an emoji.
fn rating(line: &str, kind: &mut Kind) -> Option<String> {
    let parts: Vec<&str> = line
        .split(['|', '·'])
        .map(|p| {
            p.trim()
                .trim_matches(['*', '_'])
                .trim_start_matches(|c: char| !c.is_ascii_alphabetic())
                .trim()
        })
        .filter(|p| !p.is_empty())
        .collect();
    if *kind == Kind::Comment
        && let Some(first) = parts.first()
    {
        *kind = match *first {
            "Potential issue" => Kind::Issue,
            s if s.starts_with("Refactor") || s.starts_with("Verification") => Kind::Suggestion,
            s if s.starts_with("Nitpick") => Kind::Nitpick,
            _ => Kind::Comment,
        };
    }
    parts
        .iter()
        .find(|p| SEVERITIES.contains(p))
        .map(|p| p.to_string())
}

/// The text with its agent prompt taken out, and the prompt's code block.
fn take_prompt(text: &str) -> (String, Option<String>) {
    let (Some(start), Some(end)) = (text.find(PROMPT_START), text.find(PROMPT_END)) else {
        return take_prompt_section(text);
    };
    if end < start {
        return (text.to_string(), None);
    }
    let inside = &text[start + PROMPT_START.len()..end];
    let prompt = fenced(inside).map(|(_, code)| code.trim().to_string());
    let rest = format!("{}{}", &text[..start], &text[end + PROMPT_END.len()..]);
    (rest, prompt)
}

/// The prompt from a folded "Prompt for AI Agents" section, where the
/// comment has no `ai-agent-prompt` markers round it, and the text without
/// that section.
fn take_prompt_section(text: &str) -> (String, Option<String>) {
    let Some(summary) = text.find("Prompt for AI Agents") else {
        return (text.to_string(), None);
    };
    let start = text[..summary].rfind("<details").unwrap_or(summary);
    let end = text[summary..]
        .find("</details>")
        .map_or(text.len(), |e| summary + e + "</details>".len());
    let prompt = fenced(&text[summary..end]).map(|(_, code)| code.trim().to_string());
    (format!("{}{}", &text[..start], &text[end..]), prompt)
}

/// The text with its first ```diff or ```suggestion block taken out, and
/// that block as diff lines; a suggestion is what replaces the lines, so
/// each of its lines is an addition.
fn take_fix(text: &str) -> (String, Vec<String>) {
    let mut from = 0;
    while let Some((lang, code)) = fenced(&text[from..]) {
        let open = from + text[from..].find("```").unwrap_or(0);
        let close = open + 3 + text[open + 3..].find(code).unwrap_or(0) + code.len();
        let end = text[close..]
            .find("```")
            .map_or(text.len(), |e| close + e + 3);
        let lines: Vec<String> = match lang {
            // A unified diff's own headers say nothing the place does not.
            "diff" => code
                .lines()
                .filter(|l| {
                    !["--- ", "+++ ", "@@", "diff --git", "index "]
                        .iter()
                        .any(|h| l.starts_with(h))
                })
                .map(str::to_string)
                .collect(),
            "suggestion" => code.lines().map(|l| format!("+{l}")).collect(),
            _ => {
                from = end;
                continue;
            }
        };
        return (format!("{}{}", &text[..open], &text[end..]), lines);
    }
    (text.to_string(), Vec::new())
}

/// The first fenced code block: its language and its code.
fn fenced(text: &str) -> Option<(&str, &str)> {
    let open = text.find("```")?;
    let after = &text[open + 3..];
    let newline = after.find('\n')?;
    let lang = after[..newline].trim();
    let code = &after[newline + 1..];
    let close = code.find("```")?;
    Some((lang, code[..close].trim_end_matches('\n')))
}

/// The text a reader sees: folded `<details>` sections, HTML comments and
/// tags taken out, one trimmed line per line, runs of blank lines as one.
fn plain(text: &str) -> Vec<String> {
    let text = without_details(text);
    let mut out: Vec<String> = Vec::new();
    for line in without_tags(&text).lines() {
        let line = line.trim();
        if line == "---" || (line.is_empty() && out.last().is_none_or(String::is_empty)) {
            continue;
        }
        out.push(line.to_string());
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

/// A comment's Markdown with the HTML a browser would hide or style taken
/// out: folded sections, comments and tags, though not tags written as code.
pub fn readable(text: &str) -> String {
    without_tags(&without_details(text))
}

/// `text` with its HTML comments taken out, which bots such as CodeRabbit
/// leave in a PR's description for themselves.
pub fn without_comments(text: &str) -> String {
    cut_all(text, "<!--", "-->")
}

fn without_pictures(text: &str) -> String {
    cut_all(text, "<picture>", "</picture>")
}

/// Every `<details>` section taken out, nested ones with their parent.
fn without_details(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let mut rest = text;
    loop {
        let open = rest.find("<details");
        let close = rest.find("</details>");
        match (open, close) {
            (Some(o), c) if c.is_none_or(|c| o < c) => {
                if depth == 0 {
                    out.push_str(&rest[..o]);
                }
                depth += 1;
                rest = &rest[o + "<details".len()..];
            }
            (_, Some(c)) => {
                if depth == 0 {
                    out.push_str(&rest[..c]);
                }
                depth = depth.saturating_sub(1);
                rest = &rest[c + "</details>".len()..];
            }
            _ => {
                if depth == 0 {
                    out.push_str(rest);
                }
                return out;
            }
        }
    }
}

/// HTML comments and tags taken out.
fn without_tags(text: &str) -> String {
    let text = cut_all(text, "<!--", "-->");
    text.lines()
        .map(|line| {
            // Between backticks a tag is code being talked about: kept.
            line.split('`')
                .enumerate()
                .map(|(i, part)| {
                    if i % 2 == 1 {
                        part.to_string()
                    } else {
                        strip_tags(part)
                    }
                })
                .collect::<Vec<_>>()
                .join("`")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        let tag = &rest[open + 1..];
        let is_tag = tag.starts_with(|c: char| c.is_ascii_alphabetic() || c == '/');
        match (is_tag, tag.find('>')) {
            (true, Some(end)) => {
                out.push_str(&rest[..open]);
                rest = &tag[end + 1..];
            }
            _ => {
                out.push_str(&rest[..=open]);
                rest = tag;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `text` with every span from `open` to the next `close` taken out.
fn cut_all(text: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(open) {
        out.push_str(&rest[..at]);
        match rest[at..].find(close) {
            Some(end) => rest = &rest[at + end + close.len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// A `<summary>` line's text, tags and all taken out.
fn summary(line: &str) -> Option<String> {
    line.starts_with("<summary>")
        .then(|| without_tags(line).trim().to_string())
}

/// A file heading's path: `src/a.rs: 2` or `src/a.rs (2)`.
fn file_of(summary: &str) -> String {
    let summary = summary.trim();
    let cut = summary
        .rsplit_once(": ")
        .filter(|(_, n)| n.trim().parse::<u32>().is_ok())
        .or_else(|| {
            summary
                .rsplit_once(" (")
                .filter(|(_, n)| n.trim_end_matches(')').parse::<u32>().is_ok())
        });
    cut.map_or(summary, |(path, _)| path).to_string()
}

/// A review's item opening with its lines: `` `183-188`: **Trivial** ``.
fn item_start(line: &str) -> Option<(String, &str)> {
    let rest = line.strip_prefix('`')?;
    let (lines, rest) = rest.split_once('`')?;
    let rest = rest.strip_prefix(':')?;
    let is_lines = !lines.is_empty() && lines.bytes().all(|b| b.is_ascii_digit() || b == b'-');
    // `1148-1148` is one line.
    let lines = match lines.split_once('-') {
        Some((start, end)) if start == end => start,
        _ => lines,
    };
    is_lines.then(|| (lines.to_string(), rest.trim_start()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One finding as CodeRabbit writes it now, closed by marker `id`.
    fn current(title: &str, id: &str) -> String {
        format!(
            "**<picture><img src=\"i.svg\"></picture> Minor** · **Correctness** · **Quick win**\n\n\
             **{title}**\n\nWhat is wrong, and what to do.\n\n<details>\n<summary>Supported by static analysis</summary>\n\n\
             ```bash\nrg -n x src\n```\n\n</details>\n\n<!-- ai-agent-prompt:start -->\n\n<details>\n\
             <summary>Prompt for AI Agents</summary>\n\n```\nFix {title}\n```\n\n</details>\n\n\
             <!-- ai-agent-prompt:end -->\n\n<!-- cr-indicator-types:refactor_suggestion -->\n\n<!-- cr-comment:v1:{id} -->"
        )
    }

    #[test]
    fn reads_each_finding_a_line_comment_holds_and_whether_a_commit_addressed_them() {
        let body = format!(
            "<!-- coderabbit-ai-generated:v1 -->\n\n{}\n\n---\n\n{}\n\n✅ Addressed in commit 7fb5c2c",
            current("Fit the pane.", "a"),
            current("Clear the dock.", "b")
        );
        let found = line_comment(&body);
        assert_eq!(found.len(), 2);
        let first = &found[0];
        assert_eq!(
            (first.kind, first.severity.as_deref(), first.title.as_str()),
            (Kind::Suggestion, Some("Minor"), "Fit the pane.")
        );
        assert_eq!(
            first.body, "What is wrong, and what to do.",
            "folded sections left out"
        );
        assert_eq!(first.prompt.as_deref(), Some("Fix Fit the pane."));
        assert!(found.iter().all(|f| f.resolved));
        assert_eq!(found[1].title, "Clear the dock.");
    }

    #[test]
    fn reads_the_older_format_and_takes_its_suggested_change_out_of_its_text() {
        let found = line_comment(
            "_⚠️ Potential issue_ | _🟠 Major_\n\n**Guard the empty list.**\n\n`items[0]` panics.\n\n\
             ```suggestion\nlet first = items.first()?;\n```\n\n<!-- This is an auto-generated comment by CodeRabbit -->",
        );
        let f = &found[0];
        assert_eq!(
            (
                f.kind,
                f.severity.as_deref(),
                f.title.as_str(),
                f.body.as_str()
            ),
            (
                Kind::Issue,
                Some("Major"),
                "Guard the empty list.",
                "`items[0]` panics."
            )
        );
        assert_eq!(f.fix, ["+let first = items.first()?;"]);
    }

    #[test]
    fn a_comment_in_no_shape_it_knows_still_comes_through() {
        let found = line_comment("Looks fine to me.\nOne small thing below.");
        assert_eq!(found.len(), 1);
        assert_eq!(
            (
                found[0].kind,
                found[0].title.as_str(),
                found[0].body.as_str()
            ),
            (Kind::Comment, "Looks fine to me.", "One small thing below.")
        );
    }

    #[test]
    fn reads_a_reviews_nitpicks_and_outside_diff_comments_under_their_files() {
        let body = format!(
            "**Actionable comments posted: 1**\n\n<details>\n<summary>⚠️ Outside diff range comments (1)</summary><blockquote>\n\n\
             <details>\n<summary>src/old.rs (1)</summary><blockquote>\n\n`40`: {}\n\n</blockquote></details>\n\n</blockquote></details>\n\n\
             <details>\n<summary>🧹 Nitpick comments: 1</summary><blockquote>\n\n<details>\n<summary>tests/a.test.mjs: 1</summary><blockquote>\n\n\
             `183-188`: {}\n\n</blockquote></details>\n\n</blockquote></details>\n\n\
             <details>\n<summary>Prompt to fix all issues</summary>\n\n```\n`9`: **Not** a finding\n```\n\n</details>",
            current("Close the file.", "c").replacen("refactor_suggestion", "potential_issue", 1),
            // A nitpick carries no kind of its own: its section gives it.
            current("Test the viewer.", "d").replacen(
                "<!-- cr-indicator-types:refactor_suggestion -->",
                "",
                1
            )
        );
        let found = review(&body);
        let read: Vec<_> = found
            .iter()
            .map(|f| {
                (
                    f.kind,
                    f.path.as_deref(),
                    f.lines.as_deref(),
                    f.title.as_str(),
                    f.prompt.is_some(),
                )
            })
            .collect();
        assert_eq!(
            read,
            [
                (
                    Kind::Issue,
                    Some("src/old.rs"),
                    Some("40"),
                    "Close the file.",
                    true
                ),
                (
                    Kind::Nitpick,
                    Some("tests/a.test.mjs"),
                    Some("183-188"),
                    "Test the viewer.",
                    true
                ),
            ],
            "an item's own Prompt for AI Agents heading stays inside it"
        );
    }

    #[test]
    fn a_suggested_unified_diff_keeps_only_its_lines() {
        let found = line_comment(
            "**Tighten it.**\n\n```diff\n--- \"a/AGENTS.md\"\n+++ \"b/AGENTS.md\"\n@@ -57,7 +57,8 @@\n keep\n-old\n+new\n```",
        );
        assert_eq!(found[0].fix, [" keep", "-old", "+new"]);
    }

    #[test]
    fn reads_the_heading_with_its_category_severity_and_effort() {
        let found = line_comment(
            "_⚠️ Potential issue_ | _🟠 Major_\n\n**🗄️ Data Integrity & Integration** | **🟠 Major** | **⚡ Quick win**\n\n\
             **Validate every snapshot before continuing with writes.**\n\nA bad snapshot is restored half way.",
        );
        let f = &found[0];
        assert_eq!(
            (
                f.kind,
                f.severity.as_deref(),
                f.title.as_str(),
                f.body.as_str()
            ),
            (
                Kind::Issue,
                Some("Major"),
                "Validate every snapshot before continuing with writes.",
                "A bad snapshot is restored half way."
            )
        );
    }

    #[test]
    fn keeps_tags_written_as_code() {
        let found = line_comment(
            "**Escape it.**\n\nA fenced or inline `<script>` or `<iframe>` example fails <b>validation</b>.",
        );
        assert_eq!(
            found[0].body,
            "A fenced or inline `<script>` or `<iframe>` example fails validation."
        );
    }

    #[test]
    fn a_one_line_item_names_its_line_once() {
        let found = review(
            "<details>\n<summary>Nitpick comments: 1</summary><blockquote>\n\n<details>\n<summary>src/a.hpp: 1</summary><blockquote>\n\n\
             `1148-1148`: **Trivial** · **Style**\n\n**Name it.**\n\nIt reads better.\n\n<!-- cr-comment:v1:e -->\n",
        );
        assert_eq!(found[0].lines.as_deref(), Some("1148"));
    }

    #[test]
    fn a_security_findings_labels_are_not_its_title() {
        let found = line_comment(
            "**Major** · **Security**\n\n<!-- cr-reachability -->\n**Reachability:** External\n**Exploitability:** Moderate\n\
             **CWE:** CWE-611\n<!-- /cr-reachability -->\n\n**Block XML external entities before parsing server responses.**\n\n\
             The parser resolves external entities.",
        );
        let f = &found[0];
        assert_eq!(
            f.title,
            "Block XML external entities before parsing server responses."
        );
        assert!(
            f.body.starts_with("The parser resolves external entities."),
            "{}",
            f.body
        );
        assert!(
            f.body.contains("Reachability: External"),
            "the labels stay, after the text: {}",
            f.body
        );
    }

    #[test]
    fn reads_a_title_run_into_its_text_and_a_prompt_without_markers() {
        let found = line_comment(
            "_🩺 Stability & Availability_ | _🟠 Major_ | _🏗️ Heavy lift_\n\n\
             **Wait for termination before submitting the replacement.** `ReplaceConcurrent` submits it at once.\n\n\
             <details>\n<summary>🤖 Prompt for AI Agents</summary>\n\n```\nIn operator.go wait for the old pod.\n```\n\n</details>",
        );
        let f = &found[0];
        assert_eq!(
            (f.severity.as_deref(), f.title.as_str(), f.body.as_str()),
            (
                Some("Major"),
                "Wait for termination before submitting the replacement.",
                "`ReplaceConcurrent` submits it at once."
            )
        );
        assert_eq!(
            f.prompt.as_deref(),
            Some("In operator.go wait for the old pod.")
        );
    }
}
