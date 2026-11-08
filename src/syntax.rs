//! What each run of code in the diff is, so the theme can colour it the way
//! an editor would. syntect parses with the grammars bat ships, through
//! two-face, which carries the TOML, TypeScript and Svelte syntect lacks;
//! the parse's TextMate scopes are folded into a few kinds of token, and
//! the theme gives each kind its colour.

use std::ops::Range;
use std::path::Path;
use std::sync::OnceLock;

use syntect::easy::ScopeRangeIterator;
use syntect::highlighting::ScopeSelectors;
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Keyword,
    Function,
    String,
    Constant,
    Type,
    Comment,
    Operator,
    Member,
}

/// The scopes each kind covers. Where several match, the most specific
/// wins, as it does in an editor's theme: `keyword.operator` is an
/// operator, not a keyword.
const SCOPES: [(&str, Token); 8] = [
    ("comment", Token::Comment),
    ("string, constant.character, markup.raw", Token::String),
    ("constant, support.constant", Token::Constant),
    ("keyword, storage, entity.name.tag", Token::Keyword),
    ("keyword.operator", Token::Operator),
    (
        "entity.name.function, support.function, variable.function",
        Token::Function,
    ),
    (
        "entity.name, support.type, support.class, storage.type.numeric, storage.type.primitive, markup.heading",
        Token::Type,
    ),
    (
        "variable.other.member, variable.other.property, support.macro, entity.name.macro, \
         entity.other.attribute-name, meta.annotation, markup.underline.link",
        Token::Member,
    ),
];

struct Grammars {
    syntaxes: SyntaxSet,
    scopes: Vec<(ScopeSelectors, Token)>,
}

fn grammars() -> &'static Grammars {
    static GRAMMARS: OnceLock<Grammars> = OnceLock::new();
    GRAMMARS.get_or_init(|| Grammars {
        syntaxes: two_face::syntax::extra_newlines(),
        scopes: SCOPES
            .iter()
            .map(|(scopes, token)| (scopes.parse().expect("SCOPES parse"), *token))
            .collect(),
    })
}

/// The grammar for a file, by its extension or, for files such as
/// `Makefile` or `Dockerfile`, its whole name; None for plain text.
pub fn find(path: &str) -> Option<&'static SyntaxReference> {
    let path = Path::new(path);
    let syntaxes = &grammars().syntaxes;
    [path.extension(), path.file_name()]
        .into_iter()
        .flatten()
        .filter_map(|name| name.to_str())
        .find_map(|name| syntaxes.find_syntax_by_extension(name))
        .filter(|syntax| syntax.name != "Plain Text")
}

/// Tokens line by line through one run of consecutive lines, carrying what
/// the lines before left open, such as a block comment or a string.
pub struct Highlighter {
    parse: ParseState,
    stack: ScopeStack,
}

impl Highlighter {
    pub fn new(syntax: &SyntaxReference) -> Self {
        Highlighter {
            parse: ParseState::new(syntax),
            stack: ScopeStack::new(),
        }
    }

    /// The line's tokens as char ranges, the way a side's emphasis is
    /// measured; empty when the grammar cannot parse it.
    pub fn line(&mut self, text: &str) -> Vec<(Range<usize>, Token)> {
        let grammars = grammars();
        // The grammars match line ends, so each line is given its own.
        let line = format!("{text}\n");
        let Ok(ops) = self.parse.parse_line(&line, &grammars.syntaxes) else {
            return Vec::new();
        };
        let mut tokens: Vec<(Range<usize>, Token)> = Vec::new();
        let (mut byte, mut char) = (0, 0);
        for (range, op) in ScopeRangeIterator::new(&ops, &line) {
            if self.stack.apply(op).is_err() {
                return Vec::new();
            }
            let end = range.end.min(text.len());
            if range.start >= end {
                continue;
            }
            let Some(token) = token_for(&grammars.scopes, &self.stack) else {
                continue;
            };
            char += text[byte..range.start].chars().count();
            let start = char;
            char += text[range.start..end].chars().count();
            byte = end;
            tokens.push((start..char, token));
        }
        tokens
    }
}

fn token_for(scopes: &[(ScopeSelectors, Token)], stack: &ScopeStack) -> Option<Token> {
    scopes
        .iter()
        .filter_map(|(selectors, token)| Some((selectors.does_match(stack.as_slice())?, *token)))
        .max_by(|(a, _), (b, _)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, token)| token)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The token covering `part` of `line`, found by chars as the diff
    /// measures them.
    fn token_of(tokens: &[(Range<usize>, Token)], line: &str, part: &str) -> Option<Token> {
        let start = line[..line.find(part).unwrap()].chars().count();
        let range = start..start + part.chars().count();
        tokens
            .iter()
            .find(|(r, _)| r.start <= range.start && range.end <= r.end)
            .map(|(_, token)| *token)
    }

    #[test]
    fn tokens_are_char_ranges_and_carry_what_a_line_leaves_open() {
        let mut rust = Highlighter::new(find("src/a.rs").unwrap());
        let line = r#"let s = "é"; // note"#;
        let tokens = rust.line(line);
        assert_eq!(token_of(&tokens, line, "let"), Some(Token::Keyword));
        assert_eq!(token_of(&tokens, line, "é"), Some(Token::String));
        assert_eq!(
            token_of(&tokens, line, ";"),
            None,
            "past a two-byte char, ranges count chars, not bytes"
        );
        assert_eq!(token_of(&tokens, line, "note"), Some(Token::Comment));
        rust.line("/* opens");
        let line = "still a comment */ fn f() {}";
        let tokens = rust.line(line);
        assert_eq!(token_of(&tokens, line, "still"), Some(Token::Comment));
        assert_eq!(token_of(&tokens, line, "fn"), Some(Token::Keyword));
    }

    #[test]
    fn finds_a_grammar_by_extension_or_name_and_none_for_plain_text() {
        assert!(find("web/src/App.svelte").is_some());
        assert!(find("Cargo.toml").is_some());
        assert!(find("Makefile").is_some());
        assert!(find("notes.txt").is_none());
        assert!(find("LICENSE").is_none());
    }
}
