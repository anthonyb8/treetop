//! An image in an issue, as far as it has come: on its way, refused, or
//! ready to draw at whatever size the screen gives it.

use std::cell::RefCell;

use image::DynamicImage;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;

pub(crate) enum Picture {
    Loading,
    Failed(String),
    Ready {
        /// Its size in pixels.
        width: u32,
        height: u32,
        /// Drawn through the screen's `&self`, and resized as it is drawn.
        protocol: Box<RefCell<StatefulProtocol>>,
    },
}

impl Picture {
    pub(crate) fn new(image: DynamicImage, picker: &Picker) -> Self {
        Picture::Ready {
            width: image.width(),
            height: image.height(),
            protocol: Box::new(RefCell::new(picker.new_resize_protocol(image))),
        }
    }

    /// The cells it takes in at most `columns` by `rows`, keeping its shape:
    /// at its own size, or smaller to fit. Drawn in characters, which show
    /// more of it the more of them there are, it grows to `growth` times its
    /// size where it fits; a terminal's graphics never grow it.
    pub(crate) fn fit(&self, columns: u16, rows: u16, growth: f64, picker: &Picker) -> (u16, u16) {
        let Picture::Ready { width, height, .. } = self else {
            return (0, 0);
        };
        let font = picker.font_size();
        let natural_cols = f64::from(*width) / f64::from(font.width.max(1));
        let natural_rows = f64::from(*height) / f64::from(font.height.max(1));
        let most = (f64::from(columns) / natural_cols).min(f64::from(rows) / natural_rows);
        let scale = most.min(if is_cells(picker) { growth } else { 1.0 });
        let cells = |n: f64, cap: u16| ((n * scale).ceil() as u16).clamp(1, cap.max(1));
        (cells(natural_cols, columns), cells(natural_rows, rows))
    }
}

/// Whether images draw as characters, which can be drawn in part and
/// copied, rather than as a terminal's graphics, which cannot.
pub(crate) fn is_cells(picker: &Picker) -> bool {
    picker.protocol_type() == ProtocolType::Halfblocks
}
