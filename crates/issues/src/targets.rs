//! What an open issue leads to: the issues it is linked to, the ones its
//! text mentions, and its images, in the order Tab moves through them.

use linear::{Detail, Link};

use crate::markdown;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Issue(Link),
    Image { url: String, alt: String },
}

/// Everything `detail` leads to: its links, then the issues of its team its
/// description and comments mention, then their images, each once.
pub fn targets(detail: &Detail) -> Vec<Target> {
    let mut links = detail.links.clone();
    let id = &detail.issue.id;
    let texts: Vec<&str> = detail
        .issue
        .description
        .iter()
        .map(String::as_str)
        .chain(detail.comments.iter().map(|c| c.body.as_str()))
        .collect();
    if let Some((prefix, _)) = id.split_once('-') {
        for text in &texts {
            for mentioned in mentions(text, prefix) {
                if mentioned != *id && !links.iter().any(|l| l.id == mentioned) {
                    links.push(Link {
                        kind: "mentioned".into(),
                        id: mentioned,
                        title: String::new(),
                        state: String::new(),
                    });
                }
            }
        }
    }
    let mut targets: Vec<Target> = links.into_iter().map(Target::Issue).collect();
    for text in &texts {
        for (url, alt) in markdown::images(text) {
            let is_new = !targets
                .iter()
                .any(|t| matches!(t, Target::Image { url: u, .. } if *u == url));
            if is_new {
                targets.push(Target::Image { url, alt });
            }
        }
    }
    targets
}

/// The ids of team `prefix`'s issues `text` mentions, such as `END-123`,
/// written bare or in a Linear link, each once, in order.
pub fn mentions(text: &str, prefix: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let needle = format!("{prefix}-");
    let mut from = 0;
    while let Some(at) = text[from..].find(&needle).map(|at| at + from) {
        from = at + needle.len();
        // A word of its own: nothing of a word just before it, digits after
        // the dash, and nothing of a word after those.
        let is_start = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
        let digits = text[from..].bytes().take_while(u8::is_ascii_digit).count();
        let end = from + digits;
        let is_end = bytes.get(end).is_none_or(|b| !b.is_ascii_alphanumeric());
        if is_start && digits > 0 && is_end {
            let id = text[at..end].to_string();
            if !found.contains(&id) {
                found.push(id);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use linear::{Comment, Issue};

    fn link(kind: &str, id: &str) -> Link {
        Link {
            kind: kind.into(),
            id: id.into(),
            title: format!("{id} title"),
            state: "Todo".into(),
        }
    }

    #[test]
    fn mentions_are_whole_ids_of_the_team() {
        let text = "See END-12, and END-12 again; https://linear.app/x/issue/END-40/slug. \
                    Not XEND-1, END-, END-9a or UTF-8.";
        assert_eq!(mentions(text, "END"), ["END-12", "END-40"]);
    }

    #[test]
    fn an_issue_leads_to_its_links_then_mentions_then_images() {
        let detail = Detail {
            issue: Issue {
                id: "END-1".into(),
                description: Some(
                    "Blocked on END-2, like END-3.\n\n![flow](https://u/a.png)\n\nEND-1 itself."
                        .into(),
                ),
                ..Issue::default()
            },
            comments: vec![Comment {
                author: "Bo".into(),
                body: "Same as END-5. ![](https://u/a.png) ![shot](https://u/b.png)".into(),
                created_at: String::new(),
            }],
            links: vec![link("blocked by", "END-2")],
        };
        let shape: Vec<String> = targets(&detail)
            .into_iter()
            .map(|t| match t {
                Target::Issue(l) => format!("{} {}", l.kind, l.id),
                Target::Image { url, alt } => format!("image {alt} {url}"),
            })
            .collect();
        assert_eq!(
            shape,
            [
                "blocked by END-2",
                "mentioned END-3",
                "mentioned END-5",
                "image flow https://u/a.png",
                "image shot https://u/b.png",
            ]
        );
    }
}
