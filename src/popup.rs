//! Where a popup sits: the chat, the diff and the compose box float over the
//! tree table, with a margin of it left showing round the edge, so treetop
//! stays on screen and a chat never passes for a terminal of its own. The
//! chat's terminal is sized from the same rectangle the frame is drawn in.

use ratatui::layout::Rect;

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

/// The size of a terminal drawn inside the popup, `(rows, cols)`, for a
/// screen of `rows` by `cols`.
pub fn terminal_size(rows: u16, cols: u16) -> (u16, u16) {
    let inner = inner(Rect::new(0, 0, cols, rows));
    (inner.height.max(1), inner.width.max(1))
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
        assert_eq!(terminal_size(48, 160), (38, 138));
    }

    #[test]
    fn a_small_screen_keeps_a_thin_margin_and_never_underflows() {
        assert_eq!(area(Rect::new(0, 0, 40, 10)), Rect::new(2, 1, 36, 8));
        assert_eq!(area(Rect::new(0, 0, 4, 2)), Rect::new(0, 0, 4, 2));
        assert_eq!(terminal_size(0, 0), (1, 1));
    }
}
