//! Colours by role. In a true-colour terminal they are the ones Neovim draws
//! with under gruvbox-material (medium), read from a live Neovim: lualine's
//! sections for the bars, `CursorLine` for the selected row, `WinSeparator`
//! for borders, the message groups for status, and the diff highlight groups
//! for the diff. Elsewhere, the nearest of the terminal's 16 ANSI colours.

use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};

use crate::syntax::Token;

/// Code's colours in the diff, by kind of token: Neovim's treesitter groups
/// under gruvbox-material, which italicises comments.
pub struct SyntaxPalette {
    pub keyword: Color,
    pub function: Color,
    pub string: Color,
    pub constant: Color,
    pub kind: Color,
    pub comment: Color,
    pub operator: Color,
    pub member: Color,
}

impl SyntaxPalette {
    pub fn style(&self, token: Token) -> Style {
        let color = match token {
            Token::Keyword => self.keyword,
            Token::Function => self.function,
            Token::String => self.string,
            Token::Constant => self.constant,
            Token::Type => self.kind,
            Token::Comment => return Style::new().fg(self.comment).add_modifier(Modifier::ITALIC),
            Token::Operator => self.operator,
            Token::Member => self.member,
        };
        Style::new().fg(color)
    }
}

pub struct DiffPalette {
    /// Text on every diff line; Reset is the terminal's own foreground, as
    /// Neovim's Normal is under a transparent background.
    pub text: Color,
    pub number: Color,
    /// Text and numbers on a changed line, where its background needs a
    /// different colour to stay readable; None keeps `text` and `number`.
    pub change_text: Option<Color>,
    pub removed: Color,
    pub added: Color,
    pub removed_word: Color,
    pub added_word: Color,
    /// Text on a changed word's background, where code has no colour.
    pub word_text: Color,
    /// None leaves code one colour, as the 16 ANSI colours are too few to
    /// tell a keyword from a type beside the diff's own.
    pub syntax: Option<SyntaxPalette>,
    pub hunk: Color,
    pub divider: Color,
    pub header: Style,
    pub path: Color,
}

pub struct Theme {
    pub text: Color,
    /// Secondary text: placeholders, zero counts, labels.
    pub muted: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub info: Color,
    pub accent: Color,
    pub special: Color,
    pub title: Color,
    /// The summary and key bars (lualine's `c` section).
    pub bar: Style,
    /// The name and key chips on those bars (lualine's `a` section).
    pub badge: Style,
    /// The table's header row (lualine's `b` section).
    pub header: Style,
    /// The row under the cursor (`CursorLine`).
    pub cursor: Style,
    pub border: Color,
    pub diff: DiffPalette,
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

const GRUVBOX_MATERIAL: Theme = Theme {
    text: Color::Reset,
    muted: rgb(0xa89984),
    success: rgb(0xa9b665),
    warning: rgb(0xd8a657),
    danger: rgb(0xea6962),
    info: rgb(0x7daea3),
    accent: rgb(0x89b482),
    special: rgb(0xd3869b),
    title: rgb(0xe78a4e),
    bar: Style::new().fg(rgb(0xddc7a1)).bg(rgb(0x32302f)),
    badge: Style::new()
        .fg(rgb(0x282828))
        .bg(rgb(0xa89984))
        .add_modifier(Modifier::BOLD),
    header: Style::new()
        .fg(rgb(0xddc7a1))
        .bg(rgb(0x504945))
        .add_modifier(Modifier::BOLD),
    cursor: Style::new().bg(rgb(0x7c6f64)),
    border: rgb(0x5a524c),
    diff: DiffPalette {
        text: Color::Reset,
        number: rgb(0x7c6f64),
        change_text: None,
        removed: rgb(0x402120),
        added: rgb(0x34381b),
        // A brighter shade of the line's own background rather than
        // solid red and green, so code keeps its colours on it.
        removed_word: rgb(0x6a302d),
        added_word: rgb(0x525a29),
        word_text: Color::Reset,
        syntax: Some(SyntaxPalette {
            keyword: rgb(0xea6962),
            function: rgb(0xa9b665),
            string: rgb(0x89b482),
            constant: rgb(0xd3869b),
            kind: rgb(0xd8a657),
            comment: rgb(0x928374),
            operator: rgb(0xe78a4e),
            member: rgb(0x7daea3),
        }),
        hunk: rgb(0x928374),
        divider: rgb(0x5a524c),
        header: Style::new()
            .fg(rgb(0xddc7a1))
            .bg(rgb(0x32302f))
            .add_modifier(Modifier::BOLD),
        path: rgb(0x89b482),
    },
};

const ANSI: Theme = Theme {
    text: Color::White,
    muted: Color::White,
    success: Color::Green,
    warning: Color::Yellow,
    danger: Color::Red,
    info: Color::Cyan,
    accent: Color::Cyan,
    special: Color::Magenta,
    title: Color::Yellow,
    bar: Style::new().fg(Color::White),
    badge: Style::new()
        .fg(Color::White)
        .bg(Color::Blue)
        .add_modifier(Modifier::BOLD),
    header: Style::new()
        .fg(Color::White)
        .bg(Color::Blue)
        .add_modifier(Modifier::BOLD),
    cursor: Style::new().add_modifier(Modifier::REVERSED),
    border: Color::White,
    diff: DiffPalette {
        text: Color::White,
        number: Color::Yellow,
        change_text: Some(Color::Black),
        removed: Color::Red,
        added: Color::Green,
        removed_word: Color::LightRed,
        added_word: Color::LightGreen,
        word_text: Color::Black,
        syntax: None,
        hunk: Color::Cyan,
        divider: Color::White,
        header: Style::new()
            .fg(Color::White)
            .bg(Color::Blue)
            .add_modifier(Modifier::BOLD),
        path: Color::White,
    },
};

/// The theme the terminal can show: gruvbox-material when it reports true
/// colour, as `COLORTERM` does in terminals that support 24-bit colour.
pub fn theme() -> &'static Theme {
    static IS_TRUECOLOR: OnceLock<bool> = OnceLock::new();
    let is_truecolor = *IS_TRUECOLOR.get_or_init(|| {
        std::env::var("COLORTERM").is_ok_and(|v| matches!(v.as_str(), "truecolor" | "24bit"))
    });
    if is_truecolor {
        &GRUVBOX_MATERIAL
    } else {
        &ANSI
    }
}
