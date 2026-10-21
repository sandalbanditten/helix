//! LaTeX math as Markdown has it in `$…$`, drawn rendered: commands as the symbols they stand
//! for, like `\alpha` as `α`, and scripts as superscripts and subscripts, like `x^2` as `x²`.

mod symbols;

/// The commands that set a letter in an alphabet of their own, like `\mathbb{R}` for `ℝ`.
pub(super) const ALPHABETS: &[&str] = &["\\mathbb", "\\mathcal", "\\mathscr", "\\mathfrak"];

/// What the math command `command`, like `\alpha` or `\mathbb{R}`, stands for.
pub(super) fn symbol(command: &str) -> Option<&'static str> {
    let i = symbols::SYMBOLS
        .binary_search_by_key(&command, |&(command, _)| command)
        .ok()?;
    Some(symbols::SYMBOLS[i].1)
}

/// `c` in superscript, if Unicode has it.
pub(super) fn superscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '⁰',
        '1' => '¹',
        '2' => '²',
        '3' => '³',
        '4' => '⁴',
        '5' => '⁵',
        '6' => '⁶',
        '7' => '⁷',
        '8' => '⁸',
        '9' => '⁹',
        '+' => '⁺',
        '-' => '⁻',
        '=' => '⁼',
        '(' => '⁽',
        ')' => '⁾',
        'i' => 'ⁱ',
        'n' => 'ⁿ',
        _ => return None,
    })
}

/// `c` in subscript, if Unicode has it.
pub(super) fn subscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '₀',
        '1' => '₁',
        '2' => '₂',
        '3' => '₃',
        '4' => '₄',
        '5' => '₅',
        '6' => '₆',
        '7' => '₇',
        '8' => '₈',
        '9' => '₉',
        '+' => '₊',
        '-' => '₋',
        '=' => '₌',
        '(' => '₍',
        ')' => '₎',
        'a' => 'ₐ',
        'e' => 'ₑ',
        'h' => 'ₕ',
        'i' => 'ᵢ',
        'j' => 'ⱼ',
        'k' => 'ₖ',
        'l' => 'ₗ',
        'm' => 'ₘ',
        'n' => 'ₙ',
        'o' => 'ₒ',
        'p' => 'ₚ',
        's' => 'ₛ',
        't' => 'ₜ',
        'x' => 'ₓ',
        _ => return None,
    })
}

/// The script `script` written after `^` or `_` (`mark`), if all of it can be shown so.
pub(super) fn script(mark: char, script: &str) -> Option<String> {
    let convert = match mark {
        '^' => superscript,
        _ => subscript,
    };
    if script.is_empty() {
        return None;
    }
    script.chars().map(convert).collect()
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn symbols_are_sorted() {
        assert!(symbols::SYMBOLS
            .windows(2)
            .all(|pair| pair[0].0 < pair[1].0));
    }

    #[test]
    fn lookups() {
        assert_eq!(symbol("\\alpha"), Some("α"));
        assert_eq!(symbol("\\to"), Some("→"));
        assert_eq!(symbol("\\mathbb{R}"), Some("ℝ"));
        assert_eq!(symbol("\\mathcal{L}"), Some("ℒ"));
        assert_eq!(symbol("\\sqrt"), None);
        assert_eq!(script('^', "2"), Some("²".into()));
        assert_eq!(script('_', "i=1"), Some("ᵢ₌₁".into()));
        assert_eq!(script('^', "x"), None);
    }
}
