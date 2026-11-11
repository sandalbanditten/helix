//! The logo shown when starting without files.

use helix_core::unicode::width::UnicodeWidthStr;
use helix_view::{
    graphics::{Color, Rect, Style},
    Theme,
};
use tui::buffer::Buffer as Surface;

/// From the splash screen of github.com/mattwparas/helix-config.
const ART: [&str; 13] = [
    r#"."#,
    r#"###x.        .|"#,
    r#"d#####x,   ,v||"#,
    r#" '+#####v||||||"#,
    r#"    ,v|||||+'.      _     _           _"#,
    r#" ,v|||||^'>####    | |   | |   ___   | | (_) __  __"#,
    r#"|||||^'  .v####    | |___| |  /   \  | |  _  \ \/ /"#,
    r#"||||=..v#####P'    |  ___  | /  ^  | | | | |  \  /"#,
    r#"''v'>#####P'       | |   | | |  ---  | | | |  /  \"#,
    r#",######/P||x.      |_|   |_|  \___/  |_| |_| /_/\_\"#,
    r#"####P' "x|||||,"#,
    r#"|/'       'x|||"#,
    r#" '           '|"#,
];

/// The ribbon of each char of the art's logo, `a` to `d` from the top of the logo down; the rest
/// is the name.
const RIBBONS: [&str; 13] = [
    "a",
    "aaaaa        bb",
    "aaaaaaaa   bbbb",
    " aaaaaaabbbbbbb",
    "    bbbbbbbaaa",
    " bbbbbbbbbccccc",
    "bbbbbbb  cccccc",
    "bbbbbbbcccccccc",
    "ddddcccccccc",
    "cccccccccdddd",
    "cccccc dddddddd",
    "ccc       ddddd",
    " c           dd",
];

/// The colors of the ribbons, and their stand-ins without true color.
const COLORS: [(Color, Color); 4] = [
    (Color::Rgb(0x70, 0x6b, 0xc8), Color::Magenta),
    (Color::Rgb(0x84, 0xdd, 0xea), Color::LightCyan),
    (Color::Rgb(0x99, 0x7b, 0xc8), Color::LightMagenta),
    (Color::Rgb(0x55, 0xc5, 0xe4), Color::Cyan),
];

const LOGO_WIDTH: u16 = 15;
/// Where the version goes, below the name.
const VERSION_AT: (u16, u16) = (19, 11);

/// Draws the logo and the version in the middle of `area`, without the name where it is too
/// narrow, or nothing where even that doesn't fit.
pub fn render(area: Rect, surface: &mut Surface, theme: &Theme, true_color: bool) {
    let version = match helix_loader::GIT_HASH {
        Some(hash) => format!("{} • {hash}", helix_loader::VERSION),
        None => helix_loader::VERSION.to_owned(),
    };
    let version_width = version.width() as u16;
    let rows = ART.len() as u16;
    let name_width = ART.iter().map(|line| line.len()).max().unwrap_or(0) as u16;
    let named = area.width >= name_width && area.height >= rows;
    let (width, height) = if named {
        (name_width, rows)
    } else {
        (LOGO_WIDTH.max(version_width), rows + 2)
    };
    if area.width < width || area.height < height {
        return;
    }
    let x = area.x + (area.width - width) / 2;
    let y = area.y + (area.height - height) / 2;

    let text = theme.get("ui.text");
    for (row, (line, ribbons)) in ART.iter().zip(RIBBONS).enumerate() {
        let line = if named { line } else { &line[..ribbons.len()] };
        for (col, c) in line.char_indices().filter(|(_, c)| *c != ' ') {
            let style = match ribbons.as_bytes().get(col) {
                Some(&ribbon @ b'a'..=b'd') => {
                    let (rgb, ansi) = COLORS[usize::from(ribbon - b'a')];
                    Style::default().fg(if true_color { rgb } else { ansi })
                }
                _ => text,
            };
            let c = &line[col..col + c.len_utf8()];
            surface.set_string(x + col as u16, y + row as u16, c, style);
        }
    }
    let (col, row) = if named {
        VERSION_AT
    } else {
        ((width - version_width) / 2, rows + 1)
    };
    let inactive = theme.get("ui.text.inactive");
    surface.set_string(x + col, y + row, &version, inactive);
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn rows(width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut surface = Surface::empty(area);
        render(area, &mut surface, &Theme::default(), true);
        surface
            .content
            .chunks(width.into())
            .map(|row| row.iter().map(|cell| cell.symbol.as_str()).collect())
            .collect()
    }

    #[test]
    fn narrow_areas_show_less() {
        let wide = rows(80, 20).concat();
        assert!(wide.contains(r"|_|   |_|  \___/"));
        assert!(wide.contains(helix_loader::VERSION));
        let narrow = rows(30, 20).concat();
        assert!(narrow.contains("###x."));
        assert!(!narrow.contains("|_|"));
        assert!(narrow.contains(helix_loader::VERSION));
        assert!(rows(12, 20).concat().trim().is_empty());
        assert!(rows(80, 12).concat().trim().is_empty());
    }

    /// `cargo test --release -p helix-term --lib start_screen::tests::measure -- --ignored --nocapture`
    #[test]
    #[ignore = "a measurement, not a check"]
    fn measure() {
        let area = Rect::new(0, 0, 200, 60);
        let mut surface = Surface::empty(area);
        let theme = Theme::default();
        let start = Instant::now();
        for _ in 0..10_000 {
            render(area, &mut surface, &theme, true);
        }
        println!("render: {:?}", start.elapsed() / 10_000);
    }
}
