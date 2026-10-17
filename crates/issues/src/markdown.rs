//! An issue's Markdown as styled lines: headings, lists, quotes, code, and
//! inline emphasis, code and links, so a description reads as Linear shows
//! it. Lines are left long; the pane wraps them. Images come apart from the
//! text, for the screen to draw as pictures.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::Palette;

/// A run of Markdown's lines, or an image between them.
#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    Lines(Vec<Line<'static>>),
    Image { url: String, alt: String },
}

struct Writer<'p> {
    palette: &'p Palette,
    pieces: Vec<Piece>,
    lines: Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    /// Inline styles open, innermost last.
    styles: Vec<Style>,
    /// Each open list's next number, None for a bulleted one.
    lists: Vec<Option<u64>>,
    quote: usize,
    is_code: bool,
    /// The target of the link being written, shown after its text.
    link: Option<String>,
    /// The image being read: its URL, and its alt text so far.
    image: Option<(String, String)>,
}

impl Writer<'_> {
    fn style(&self) -> Style {
        self.styles
            .iter()
            .fold(Style::new(), |style, more| style.patch(*more))
    }

    /// The indent a new line starts with: quote bars, then list depth.
    fn lead(&self) -> Vec<Span<'static>> {
        let mut lead = Vec::new();
        if self.quote > 0 {
            lead.push(Span::styled(
                "│ ".repeat(self.quote),
                Style::new().fg(self.palette.muted),
            ));
        }
        let depth = self.lists.len().saturating_sub(1);
        if depth > 0 {
            lead.push(Span::raw("  ".repeat(depth)));
        }
        lead
    }

    fn push(&mut self, text: &str) {
        // A line starts at its text, as one after an image would not.
        let text = if self.spans.is_empty() {
            text.trim_start()
        } else {
            text
        };
        if text.is_empty() {
            return;
        }
        if self.spans.is_empty() {
            let lead = self.lead();
            self.spans.extend(lead);
        }
        self.spans
            .push(Span::styled(text.to_string(), self.style()));
    }

    fn end_line(&mut self) {
        if let Some(last) = self.spans.last_mut()
            && last.content.ends_with(' ')
        {
            last.content = last.content.trim_end().to_string().into();
        }
        if !self.spans.is_empty() {
            self.lines.push(Line::from(std::mem::take(&mut self.spans)));
        }
    }

    fn blank(&mut self) {
        self.end_line();
        if self.lines.last().is_some_and(|l| l.width() > 0) {
            self.lines.push(Line::default());
        }
    }

    /// Ends the lines so far as a piece of their own.
    fn cut(&mut self) {
        self.end_line();
        while self.lines.last().is_some_and(|l| l.width() == 0) {
            self.lines.pop();
        }
        if !self.lines.is_empty() {
            self.pieces
                .push(Piece::Lines(std::mem::take(&mut self.lines)));
        }
    }

    fn event(&mut self, event: Event) {
        let p = self.palette;
        if let Some((_, alt)) = &mut self.image {
            match event {
                Event::End(TagEnd::Image) => {
                    let (url, alt) = self.image.take().unwrap_or_default();
                    self.cut();
                    self.pieces.push(Piece::Image { url, alt });
                }
                Event::Text(text) | Event::Code(text) => alt.push_str(&text),
                _ => {}
            }
            return;
        }
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                self.blank();
                let color = if level == HeadingLevel::H1 {
                    p.title
                } else {
                    p.accent
                };
                self.styles
                    .push(Style::new().fg(color).add_modifier(Modifier::BOLD));
            }
            Event::End(TagEnd::Heading(_)) => {
                self.styles.pop();
                self.blank();
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                if self.lists.is_empty() {
                    self.blank();
                } else {
                    self.end_line();
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                self.end_line();
                self.quote += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                self.end_line();
                self.quote = self.quote.saturating_sub(1);
                self.blank();
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                self.blank();
                self.is_code = true;
                if let CodeBlockKind::Fenced(lang) = kind
                    && !lang.is_empty()
                {
                    self.lines.push(Line::styled(
                        format!("  {lang}"),
                        Style::new().fg(p.muted).add_modifier(Modifier::ITALIC),
                    ));
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                self.is_code = false;
                self.blank();
            }
            Event::Start(Tag::List(start)) => {
                self.end_line();
                self.lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                self.end_line();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.blank();
                }
            }
            Event::Start(Tag::Item) => {
                self.end_line();
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let marker = format!("{n}. ");
                        *n += 1;
                        marker
                    }
                    _ => "• ".to_string(),
                };
                let lead = self.lead();
                self.spans.extend(lead);
                self.spans
                    .push(Span::styled(marker, Style::new().fg(p.accent)));
            }
            Event::End(TagEnd::Item) => self.end_line(),
            Event::TaskListMarker(done) => {
                self.push(if done { "[x] " } else { "[ ] " });
            }
            Event::Start(Tag::Emphasis) => self
                .styles
                .push(Style::new().add_modifier(Modifier::ITALIC)),
            Event::Start(Tag::Strong) => {
                self.styles.push(Style::new().add_modifier(Modifier::BOLD))
            }
            Event::Start(Tag::Strikethrough) => self
                .styles
                .push(Style::new().add_modifier(Modifier::CROSSED_OUT)),
            Event::End(TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough) => {
                self.styles.pop();
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                self.link = Some(dest_url.to_string());
                self.styles
                    .push(Style::new().fg(p.info).add_modifier(Modifier::UNDERLINED));
            }
            Event::End(TagEnd::Link) => {
                self.styles.pop();
                if let Some(url) = self.link.take()
                    && !self.spans.iter().any(|s| s.content == url)
                {
                    self.spans
                        .push(Span::styled(format!(" ({url})"), Style::new().fg(p.muted)));
                }
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                self.image = Some((dest_url.to_string(), String::new()));
            }
            Event::Text(text) if self.is_code => {
                for line in text.lines() {
                    self.lines
                        .push(Line::styled(format!("    {line}"), Style::new().fg(p.info)));
                }
            }
            Event::Text(text) => self.push(&text),
            Event::Code(code) => {
                if self.spans.is_empty() {
                    let lead = self.lead();
                    self.spans.extend(lead);
                }
                self.spans
                    .push(Span::styled(code.to_string(), Style::new().fg(p.info)));
            }
            Event::SoftBreak => self.push(" "),
            Event::HardBreak => self.end_line(),
            Event::Rule => {
                self.blank();
                self.lines
                    .push(Line::styled("─".repeat(24), Style::new().fg(p.border)));
                self.blank();
            }
            Event::Html(html) | Event::InlineHtml(html) => self.push(&html),
            _ => {}
        }
    }
}

/// `markdown` as runs of lines and the images between them, with no blank
/// line at either end of a run.
pub fn pieces(markdown: &str, palette: &Palette) -> Vec<Piece> {
    let mut writer = Writer {
        palette,
        pieces: Vec::new(),
        lines: Vec::new(),
        spans: Vec::new(),
        styles: Vec::new(),
        lists: Vec::new(),
        quote: 0,
        is_code: false,
        link: None,
        image: None,
    };
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(markdown, options) {
        writer.event(event);
    }
    writer.cut();
    writer.pieces
}

/// An image's line where only text fits: a mark and its name.
pub fn image_line(alt: &str, palette: &Palette) -> Line<'static> {
    let name = if alt.trim().is_empty() {
        "image"
    } else {
        alt.trim()
    };
    Line::styled(format!("▣ {name}"), Style::new().fg(palette.special))
}

/// `markdown` as lines, each image a line naming it, with no blank line at
/// either end.
pub fn render(markdown: &str, palette: &Palette) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for piece in pieces(markdown, palette) {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        match piece {
            Piece::Lines(more) => lines.extend(more),
            Piece::Image { alt, .. } => lines.push(image_line(&alt, palette)),
        }
    }
    lines
}

/// The images in `markdown`, by URL and alt text, in order.
pub fn images(markdown: &str) -> Vec<(String, String)> {
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut found = Vec::new();
    let mut open: Option<(String, String)> = None;
    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Start(Tag::Image { dest_url, .. }) => {
                open = Some((dest_url.to_string(), String::new()));
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some((_, alt)) = &mut open {
                    alt.push_str(&text);
                }
            }
            Event::End(TagEnd::Image) => found.extend(open.take()),
            _ => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn renders_headings_lists_code_and_links() {
        let markdown = "# Login\n\nAfter **SSO** the user lands on `/home`.\n\n\
            - keep `return_to`\n- test it\n  1. once\n  2. twice\n\n\
            ```rust\nlet x = 1;\n```\n\nSee [the doc](https://x.dev).";
        assert_eq!(
            text(&render(markdown, &Palette::default())),
            [
                "Login",
                "",
                "After SSO the user lands on /home.",
                "",
                "• keep return_to",
                "• test it",
                "  1. once",
                "  2. twice",
                "",
                "  rust",
                "    let x = 1;",
                "",
                "See the doc (https://x.dev).",
            ]
        );
    }

    #[test]
    fn images_come_apart_from_the_text_around_them() {
        let markdown = "Before.\n\n![login page](https://uploads.linear.app/a.png)\n\nAfter, \
            with ![](https://x.dev/b.png) inline.";
        let pieces = pieces(markdown, &Palette::default());
        let shape: Vec<String> = pieces
            .iter()
            .map(|p| match p {
                Piece::Lines(lines) => text(lines).join("|"),
                Piece::Image { url, alt } => format!("image {alt} {url}"),
            })
            .collect();
        assert_eq!(
            shape,
            [
                "Before.",
                "image login page https://uploads.linear.app/a.png",
                "After, with",
                "image  https://x.dev/b.png",
                "inline.",
            ]
        );
        assert_eq!(
            images(markdown),
            [
                (
                    "https://uploads.linear.app/a.png".to_string(),
                    "login page".to_string()
                ),
                ("https://x.dev/b.png".to_string(), String::new()),
            ]
        );
        assert_eq!(
            text(&render(markdown, &Palette::default())),
            [
                "Before.",
                "",
                "▣ login page",
                "",
                "After, with",
                "",
                "▣ image",
                "",
                "inline."
            ]
        );
    }

    #[test]
    fn a_bare_link_is_not_written_twice_and_a_quote_keeps_its_bar() {
        let lines = render("<https://x.dev>\n\n> asked twice", &Palette::default());
        assert_eq!(text(&lines), ["https://x.dev", "", "│ asked twice"]);
    }
}
