//! Select Graphic Rendition parameters, like the `01;31` of `ESC [ 01;31 m`, as styles.

use helix_view::graphics::{Color, Modifier, Style, UnderlineStyle};

/// Applies the SGR parameters `params` to `style`, as a terminal does.
pub fn apply(mut style: Style, params: &str) -> Style {
    if params.is_empty() {
        return Style::default();
    }
    let mut codes = params.split(';');
    while let Some(code) = codes.next() {
        // Parameters may have sub-parameters after colons, like `4:3` or `38:2::255:0:0`.
        let mut parts = code.split(':');
        let first = parts.next().unwrap_or_default().trim();
        // An empty parameter counts as `0`.
        let number = if first.is_empty() {
            0
        } else if let Ok(number) = first.parse::<u16>() {
            number
        } else {
            continue;
        };
        let add = |style: &mut Style, modifier| style.add_modifier.insert(modifier);
        let remove = |style: &mut Style, modifier| style.add_modifier.remove(modifier);
        match number {
            0 => style = Style::default(),
            1 => add(&mut style, Modifier::BOLD),
            2 => add(&mut style, Modifier::DIM),
            3 => add(&mut style, Modifier::ITALIC),
            4 => {
                style.underline_style = match parts.next() {
                    Some("0") => None,
                    Some("2") => Some(UnderlineStyle::DoubleLine),
                    Some("3") => Some(UnderlineStyle::Curl),
                    Some("4") => Some(UnderlineStyle::Dotted),
                    Some("5") => Some(UnderlineStyle::Dashed),
                    _ => Some(UnderlineStyle::Line),
                }
            }
            5 => add(&mut style, Modifier::SLOW_BLINK),
            6 => add(&mut style, Modifier::RAPID_BLINK),
            7 => add(&mut style, Modifier::REVERSED),
            8 => add(&mut style, Modifier::HIDDEN),
            9 => add(&mut style, Modifier::CROSSED_OUT),
            21 => style.underline_style = Some(UnderlineStyle::DoubleLine),
            22 => remove(&mut style, Modifier::BOLD | Modifier::DIM),
            23 => remove(&mut style, Modifier::ITALIC),
            24 => style.underline_style = None,
            25 => remove(&mut style, Modifier::SLOW_BLINK | Modifier::RAPID_BLINK),
            27 => remove(&mut style, Modifier::REVERSED),
            28 => remove(&mut style, Modifier::HIDDEN),
            29 => remove(&mut style, Modifier::CROSSED_OUT),
            30..=37 => style.fg = Some(Color::Indexed((number - 30) as u8)),
            38 => style.fg = extended(&mut parts, &mut codes).or(style.fg),
            39 => style.fg = None,
            40..=47 => style.bg = Some(Color::Indexed((number - 40) as u8)),
            48 => style.bg = extended(&mut parts, &mut codes).or(style.bg),
            49 => style.bg = None,
            58 => {
                style.underline_color = extended(&mut parts, &mut codes).or(style.underline_color)
            }
            59 => style.underline_color = None,
            90..=97 => style.fg = Some(Color::Indexed((number - 82) as u8)),
            100..=107 => style.bg = Some(Color::Indexed((number - 92) as u8)),
            _ => {}
        }
    }
    style
}

/// Reads the color of `38`, `48` or `58`: `5;index` or `2;red;green;blue`.
fn extended<'a>(
    parts: &mut impl Iterator<Item = &'a str>,
    codes: &mut impl Iterator<Item = &'a str>,
) -> Option<Color> {
    let parts: Vec<&str> = parts.collect();
    let number = |value: &str| value.trim().parse::<u8>().ok();
    if let Some((kind, rest)) = parts.split_first() {
        return match *kind {
            "5" => Some(Color::Indexed(number(rest.first()?)?)),
            "2" => match rest {
                [.., red, green, blue] => {
                    Some(Color::Rgb(number(red)?, number(green)?, number(blue)?))
                }
                _ => None,
            },
            _ => None,
        };
    }
    match codes.next()?.trim() {
        "5" => Some(Color::Indexed(number(codes.next()?)?)),
        "2" => {
            let (red, green, blue) = (codes.next()?, codes.next()?, codes.next()?);
            Some(Color::Rgb(number(red)?, number(green)?, number(blue)?))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fg(color: Color) -> Style {
        Style::default().fg(color)
    }

    #[test]
    fn parameters_apply_one_after_another() {
        let red = apply(Style::default(), "31");
        assert_eq!(red, fg(Color::Indexed(1)));
        // cargo writes bold and the color apart, then resets.
        let bold_red = apply(red, "1");
        assert_eq!(bold_red, fg(Color::Indexed(1)).add_modifier(Modifier::BOLD));
        assert_eq!(apply(bold_red, "0"), Style::default());
        assert_eq!(apply(bold_red, ""), Style::default());
        assert_eq!(apply(bold_red, "22;39"), Style::default());
        assert_eq!(apply(bold_red, ";32"), fg(Color::Indexed(2)));
    }

    #[test]
    fn colors_of_the_palette_and_beyond() {
        let none = Style::default();
        assert_eq!(apply(none, "92"), fg(Color::Indexed(10)));
        assert_eq!(apply(none, "38;5;208"), fg(Color::Indexed(208)));
        assert_eq!(apply(none, "38;2;69;133;136"), fg(Color::Rgb(69, 133, 136)));
        assert_eq!(apply(none, "38:2::1:2:3"), fg(Color::Rgb(1, 2, 3)));
        assert_eq!(apply(none, "38:5:4"), fg(Color::Indexed(4)));
        assert_eq!(
            apply(none, "41;30"),
            Style::default().bg(Color::Indexed(1)).fg(Color::Indexed(0))
        );
        assert_eq!(apply(none, "104"), Style::default().bg(Color::Indexed(12)));
        assert_eq!(
            apply(none, "4:3;58;5;1"),
            Style::default()
                .underline_style(UnderlineStyle::Curl)
                .underline_color(Color::Indexed(1))
        );
        // Unknown and broken parameters are left out.
        assert_eq!(apply(none, "38;9;31"), fg(Color::Indexed(1)));
        assert_eq!(apply(none, "x;33"), fg(Color::Indexed(3)));
    }
}
