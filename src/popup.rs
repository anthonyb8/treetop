//! Where a popup sits: the chat, the diff and the compose box float over the
//! tree table, with a margin of it left showing round the edge, so treetop
//! stays on screen and a chat never passes for a terminal of its own. The
//! chat and the diff say what they show in a bar on the first row inside the
//! border, and the chat's terminal is sized from the rows under it, less
//! the sidebar at its right where the popup is wide enough for one.

use ratatui::layout::Rect;

/// How wide a chat's sidebar is, its border included.
const SIDEBAR: u16 = 30;

/// How wide the popup's body must be for a chat to have a sidebar: below
/// it, the chat gets every column.
const SIDEBAR_FROM: u16 = 100;

/// The popup over a screen of `screen`: a margin of about a sixteenth of the
/// width at each side and a twelfth of the height at the top and bottom, at
/// least two columns and one row while the screen has room for them.
pub fn area(screen: Rect) -> Rect {
    let margin_x = (screen.width / 16).max(2).min(screen.width / 8);
    let margin_y = (screen.height / 12).max(1).min(screen.height / 8);
    Rect::new(
        screen.x + margin_x,
        screen.y + margin_y,
        screen.width.saturating_sub(2 * margin_x),
        screen.height.saturating_sub(2 * margin_y),
    )
}

/// Inside the popup's border, where its content goes.
pub fn inner(screen: Rect) -> Rect {
    let outer = area(screen);
    Rect::new(
        outer.x + 1,
        outer.y + 1,
        outer.width.saturating_sub(2),
        outer.height.saturating_sub(2),
    )
}

/// The bar on the first row inside the popup's border.
pub fn bar(screen: Rect) -> Rect {
    let inner = inner(screen);
    Rect::new(inner.x, inner.y, inner.width, inner.height.min(1))
}

/// Inside the popup's border, under its bar: where a chat's terminal or a
/// diff goes.
pub fn body(screen: Rect) -> Rect {
    let inner = inner(screen);
    Rect::new(
        inner.x,
        inner.y + inner.height.min(1),
        inner.width,
        inner.height.saturating_sub(1),
    )
}

/// The chat's sidebar at the right of the body, where it is wide enough
/// for one.
pub fn sidebar(screen: Rect) -> Option<Rect> {
    let body = body(screen);
    (body.width >= SIDEBAR_FROM)
        .then(|| Rect::new(body.right() - SIDEBAR, body.y, SIDEBAR, body.height))
}

/// Where a chat's terminal goes: the body, less its sidebar.
pub fn chat(screen: Rect) -> Rect {
    let body = body(screen);
    let width = sidebar(screen).map_or(0, |side| side.width);
    Rect {
        width: body.width - width,
        ..body
    }
}

/// The size of a chat's terminal, `(rows, cols)`, for a screen of `rows` by
/// `cols`.
pub fn terminal_size(rows: u16, cols: u16) -> (u16, u16) {
    let chat = chat(Rect::new(0, 0, cols, rows));
    (chat.height.max(1), chat.width.max(1))
}

/// A box of `width` by `height` in the middle of `area`, shrunk to fit.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_the_table_showing_round_the_edge() {
        let screen = Rect::new(0, 0, 160, 48);
        assert_eq!(area(screen), Rect::new(10, 4, 140, 40));
        assert_eq!(inner(screen), Rect::new(11, 5, 138, 38));
        assert_eq!(bar(screen), Rect::new(11, 5, 138, 1));
        assert_eq!(body(screen), Rect::new(11, 6, 138, 37));
    }

    #[test]
    fn a_chat_shares_a_wide_popup_with_its_sidebar_and_fills_a_narrow_one() {
        let wide = Rect::new(0, 0, 160, 48);
        assert_eq!(sidebar(wide), Some(Rect::new(119, 6, 30, 37)));
        assert_eq!(chat(wide), Rect::new(11, 6, 108, 37));
        assert_eq!(terminal_size(48, 160), (37, 108));
        let narrow = Rect::new(0, 0, 100, 30);
        assert_eq!(sidebar(narrow), None);
        assert_eq!(chat(narrow), body(narrow));
    }

    #[test]
    fn a_small_screen_keeps_a_thin_margin_and_never_underflows() {
        assert_eq!(area(Rect::new(0, 0, 40, 10)), Rect::new(2, 1, 36, 8));
        assert_eq!(area(Rect::new(0, 0, 4, 2)), Rect::new(0, 0, 4, 2));
        assert_eq!(body(Rect::new(0, 0, 4, 2)), Rect::new(1, 1, 2, 0));
        assert_eq!(terminal_size(0, 0), (1, 1));
    }
}
