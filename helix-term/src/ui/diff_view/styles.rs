//! The colors of the diff view, from the theme's `diff.plus.line`, `diff.plus.text`,
//! `diff.minus.line`, `diff.minus.text` and `diff.filler`, or blended from `diff.plus`,
//! `diff.minus` and `ui.text`.

use helix_view::{
    diff_view::Side,
    graphics::{Color, Style},
    Theme,
};

/// How much of the diff color a changed line's background has, and the changed text's.
const LINE_BLEND: f32 = 0.19;
const TEXT_BLEND: f32 = 0.38;
/// How much of the text color the fillers' background has.
const FILLER_BLEND: f32 = 0.1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Styles {
    plus_line: Style,
    plus_text: Style,
    minus_line: Style,
    minus_text: Style,
    pub filler: Style,
    /// The character drawn over the fillers' background.
    pub filler_character: Style,
}

impl Styles {
    pub fn new(theme: &Theme) -> Self {
        let background = rgb(theme.get("ui.background").bg).unwrap_or(if is_dark(theme) {
            (0, 0, 0)
        } else {
            (255, 255, 255)
        });
        let color = |scope: &str, fallback| rgb(theme.get(scope).fg).unwrap_or(fallback);
        let plus = color("diff.plus", (0x9e, 0xce, 0x6a));
        let minus = color("diff.minus", (0xf7, 0x76, 0x8e));
        let text = color(
            "ui.text",
            if is_dark(theme) {
                (255, 255, 255)
            } else {
                (0, 0, 0)
            },
        );
        // Looked up exactly: `diff.plus` itself colors text.
        let scope = |scope: &str, color, amount| {
            theme
                .try_get_exact(scope)
                .unwrap_or_else(|| Style::default().bg(blend(color, background, amount)))
        };
        let indent_guide = theme
            .try_get("ui.virtual.indent-guide")
            .unwrap_or_else(|| theme.get("ui.virtual.whitespace"));
        let filler_character = theme
            .try_get_exact("diff.filler")
            .and_then(|style| style.fg)
            .or(indent_guide.fg);
        Self {
            plus_line: scope("diff.plus.line", plus, LINE_BLEND),
            plus_text: scope("diff.plus.text", plus, TEXT_BLEND),
            minus_line: scope("diff.minus.line", minus, LINE_BLEND),
            minus_text: scope("diff.minus.text", minus, TEXT_BLEND),
            filler: scope("diff.filler", text, FILLER_BLEND),
            filler_character: Style {
                fg: filler_character,
                ..Style::default()
            },
        }
    }

    /// The style of a changed line of `side`: removed on the old side, added on the new one.
    pub fn line(&self, side: Side) -> Style {
        match side {
            Side::Old => self.minus_line,
            Side::New => self.plus_line,
        }
    }

    /// The style of the text that changed within a line of `side`.
    pub fn text(&self, side: Side) -> Style {
        match side {
            Side::Old => self.minus_text,
            Side::New => self.plus_text,
        }
    }
}

/// `amount` of `color` over `background`.
fn blend(color: (u8, u8, u8), background: (u8, u8, u8), amount: f32) -> Color {
    let mix = |color: u8, background: u8| {
        (f32::from(color) * amount + f32::from(background) * (1.0 - amount)).round() as u8
    };
    Color::Rgb(
        mix(color.0, background.0),
        mix(color.1, background.1),
        mix(color.2, background.2),
    )
}

/// Whether the theme's background is dark, which also picks `difft`'s colors.
pub fn is_dark(theme: &Theme) -> bool {
    match rgb(theme.get("ui.background").bg) {
        Some((r, g, b)) => {
            (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000 < 128
        }
        None => true,
    }
}

/// The red, green and blue of `color`. `None` for the terminal's own colors.
fn rgb(color: Option<Color>) -> Option<(u8, u8, u8)> {
    const ANSI: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
        (127, 127, 127),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (92, 92, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    let indexed = |index: u8| match index {
        0..16 => ANSI[index as usize],
        16..232 => {
            let level = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            let index = index - 16;
            (level(index / 36), level(index / 6 % 6), level(index % 6))
        }
        232.. => {
            let gray = 8 + (index - 232) * 10;
            (gray, gray, gray)
        }
    };
    Some(match color? {
        Color::Reset => return None,
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(index) => indexed(index),
        Color::Black => ANSI[0],
        Color::Red => ANSI[1],
        Color::Green => ANSI[2],
        Color::Yellow => ANSI[3],
        Color::Blue => ANSI[4],
        Color::Magenta => ANSI[5],
        Color::Cyan => ANSI[6],
        Color::Gray => ANSI[8],
        Color::LightRed => ANSI[9],
        Color::LightGreen => ANSI[10],
        Color::LightYellow => ANSI[11],
        Color::LightBlue => ANSI[12],
        Color::LightMagenta => ANSI[13],
        Color::LightCyan => ANSI[14],
        Color::LightGray => ANSI[7],
        Color::White => ANSI[15],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(toml: &str) -> Theme {
        Theme::from(toml::from_str::<toml::Value>(toml).unwrap())
    }

    #[test]
    fn themes_without_the_scopes_get_blends_of_their_colors() {
        let theme = theme(
            r##"
"ui.background" = { bg = "#000000" }
"ui.text" = "#ffffff"
"diff.plus" = "#00ff00"
"diff.minus" = "#ff0000"
"##,
        );
        let styles = Styles::new(&theme);
        assert_eq!(styles.line(Side::New).bg, Some(Color::Rgb(0, 48, 0)));
        assert_eq!(styles.text(Side::New).bg, Some(Color::Rgb(0, 97, 0)));
        assert_eq!(styles.line(Side::Old).bg, Some(Color::Rgb(48, 0, 0)));
        assert_eq!(styles.filler.bg, Some(Color::Rgb(26, 26, 26)));
    }

    #[test]
    fn the_theme_s_scopes_win() {
        let theme = theme(
            r##"
"diff.plus" = "#00ff00"
"diff.plus.line" = { bg = "#123456" }
"diff.filler" = { bg = "#222222", fg = "#333333" }
"##,
        );
        let styles = Styles::new(&theme);
        assert_eq!(
            styles.line(Side::New).bg,
            Some(Color::Rgb(0x12, 0x34, 0x56))
        );
        assert_eq!(styles.filler.fg, Some(Color::Rgb(0x33, 0x33, 0x33)));
        // Without a background the terminal's counts as dark.
        assert_eq!(styles.text(Side::New).bg, Some(Color::Rgb(0, 97, 0)));
    }

    #[test]
    fn palette_colors_have_their_xterm_values() {
        assert_eq!(rgb(Some(Color::Indexed(1))), Some((205, 0, 0)));
        assert_eq!(rgb(Some(Color::Indexed(196))), Some((255, 0, 0)));
        assert_eq!(rgb(Some(Color::Indexed(16))), Some((0, 0, 0)));
        assert_eq!(rgb(Some(Color::Indexed(244))), Some((128, 128, 128)));
        assert_eq!(rgb(Some(Color::Reset)), None);
        assert_eq!(rgb(None), None);
    }
}
