//! treetop's Issues screen: a ticket system's issues on top, narrowed by a
//! filter line, and the one under the cursor below with its description and
//! comments. It knows nothing about trees; the program it runs in hands it
//! its sources as backends and acts on what the screen asks for.

mod backend;
mod draw;
mod markdown;
mod screen;
mod views;
mod worker;

use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

pub use backend::{Backend, Linear, READ_ONLY};
pub use screen::{Action, Screen};
pub use views::{Group, View};

/// The colours the screen draws in, given by the program it runs in so the
/// two look like one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub text: Color,
    pub muted: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub info: Color,
    pub accent: Color,
    pub special: Color,
    pub title: Color,
    pub bar: Style,
    pub badge: Style,
    pub header: Style,
    pub cursor: Style,
    pub border: Color,
}

impl Default for Palette {
    /// The terminal's own sixteen colours.
    fn default() -> Self {
        Palette {
            text: Color::Reset,
            muted: Color::Gray,
            success: Color::Green,
            warning: Color::Yellow,
            danger: Color::Red,
            info: Color::Cyan,
            accent: Color::Cyan,
            special: Color::Magenta,
            title: Color::Yellow,
            bar: Style::new().add_modifier(Modifier::REVERSED),
            badge: Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            header: Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            cursor: Style::new().add_modifier(Modifier::REVERSED),
            border: Color::DarkGray,
        }
    }
}

/// What narrows a list of issues. Each is optional; none lists the team's open issues.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filters {
    /// Only issues assigned to you.
    #[serde(default)]
    pub mine: bool,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    /// `current` for the active cycle, else a cycle number.
    #[serde(default)]
    pub cycle: Option<String>,
    /// Words the title contains.
    #[serde(default)]
    pub query: Option<String>,
}

/// The filter line's words, split at spaces outside double quotes.
fn words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let (mut word, mut is_quoted) = (String::new(), false);
    for c in text.chars() {
        match c {
            '"' => is_quoted = !is_quoted,
            c if c.is_whitespace() && !is_quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// The filters a filter line asks for: `mine`, `label:`, `state:`,
/// `project:` and `cycle:` (`current` or a number) take a value, quoted when
/// it has spaces; every other word searches titles.
pub fn parse(text: &str) -> Filters {
    let mut filters = Filters::default();
    let mut query = Vec::new();
    for word in words(text) {
        let (key, value) = word.split_once(':').unwrap_or(("", &word));
        let value = Some(value.to_string()).filter(|v| !v.is_empty());
        match key {
            "label" => filters.label = value,
            "state" => filters.state = value,
            "project" => filters.project = value,
            "cycle" => filters.cycle = value,
            _ if word == "mine" => filters.mine = true,
            _ => query.push(word),
        }
    }
    filters.query = (!query.is_empty()).then(|| query.join(" "));
    filters
}

/// `filters` as the filter line that asks for them, which `parse` reads back.
pub fn line(filters: &Filters) -> String {
    let quoted = |value: &str| {
        if value.contains(' ') {
            format!("\"{value}\"")
        } else {
            value.to_string()
        }
    };
    let mut parts = Vec::new();
    if filters.mine {
        parts.push("mine".to_string());
    }
    for (key, value) in [
        ("label", &filters.label),
        ("state", &filters.state),
        ("project", &filters.project),
        ("cycle", &filters.cycle),
    ] {
        if let Some(value) = value {
            parts.push(format!("{key}:{}", quoted(value)));
        }
    }
    parts.extend(filters.query.clone());
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filter_line_reads_keys_quoted_values_and_search_words() {
        let filters = parse(r#"mine label:Bug state:"In Review" cycle:current fix login"#);
        assert_eq!(
            filters,
            Filters {
                mine: true,
                label: Some("Bug".into()),
                state: Some("In Review".into()),
                cycle: Some("current".into()),
                query: Some("fix login".into()),
                ..Filters::default()
            }
        );
        assert_eq!(parse(&line(&filters)), filters, "it reads back");
        assert_eq!(parse("  "), Filters::default());
    }
}
