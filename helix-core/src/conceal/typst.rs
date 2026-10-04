//! Typst's symbols, looked up the way Typst resolves them. The symbols are those of the `codex`
//! crate that Typst itself uses.

use codex::{Def, ModifierSet, Module};

/// Typst's shorthands of more than one character and the characters they stand for.
const SHORTHANDS: &[(&str, &str)] = &[
    // markup and math
    ("...", "…"),
    // markup
    ("--", "–"),
    ("---", "—"),
    // math
    ("!=", "≠"),
    (":=", "≔"),
    ("::=", "⩴"),
    ("=:", "≕"),
    ("<<", "≪"),
    ("<<<", "⋘"),
    (">>", "≫"),
    (">>>", "⋙"),
    ("<=", "≤"),
    (">=", "≥"),
    ("->", "→"),
    ("-->", "⟶"),
    ("|->", "↦"),
    (">->", "↣"),
    ("->>", "↠"),
    ("<-", "←"),
    ("<--", "⟵"),
    ("<-<", "↢"),
    ("<<-", "↞"),
    ("<->", "↔"),
    ("<-->", "⟷"),
    ("~>", "⇝"),
    ("~~>", "⟿"),
    ("<~", "⇜"),
    ("<~~", "⬳"),
    ("=>", "⇒"),
    ("|=>", "⤇"),
    ("==>", "⟹"),
    ("<==", "⟸"),
    ("<=>", "⇔"),
    ("<==>", "⟺"),
    ("[|", "⟦"),
    ("|]", "⟧"),
    ("||", "‖"),
];

/// The symbol that `path`, a name with modifiers like `arrow.r.long`, stands for in math.
pub(super) fn math_symbol(path: &str) -> Option<&'static str> {
    resolve(codex::SYM, path)
}

/// The symbol or emoji that `code`, like `sym.qed` or `#emoji.face`, stands for.
pub(super) fn code_symbol(code: &str) -> Option<&'static str> {
    resolve(codex::ROOT, code.strip_prefix('#').unwrap_or(code))
}

/// The char that `shorthand`, like `->` or `--`, stands for.
pub(super) fn shorthand(shorthand: &str) -> Option<&'static str> {
    SHORTHANDS
        .iter()
        .find(|&&(text, _)| text == shorthand)
        .map(|&(_, symbol)| symbol)
}

/// Resolves `path` in `module` like Typst does, with modifiers in any order.
fn resolve(mut module: Module, path: &str) -> Option<&'static str> {
    if path.split('.').any(str::is_empty) {
        return None;
    }
    let mut path = path;
    loop {
        let (name, modifiers) = path.split_once('.').unwrap_or((path, ""));
        match module.get(name)?.def {
            Def::Module(submodule) if !modifiers.is_empty() => {
                module = submodule;
                path = modifiers;
            }
            Def::Module(_) => return None,
            Def::Symbol(symbol) => {
                let (symbol, _deprecation) = symbol.get(ModifierSet::from_raw_dotted(modifiers))?;
                return Some(symbol);
            }
        }
    }
}

#[cfg(test)]
mod test {
    use unicode_segmentation::UnicodeSegmentation;

    use super::*;
    use crate::conceal::{is_visible, MAX_CONCEALED_BYTES};

    /// Calls `f` with the full path, like `arrow.r.long`, and the value of every variant of every
    /// symbol in `module`.
    fn for_each_variant(module: Module, prefix: &str, f: &mut impl FnMut(&str, &'static str)) {
        for (name, binding) in module.iter() {
            match binding.def {
                Def::Module(submodule) => {
                    for_each_variant(submodule, &format!("{prefix}{name}."), f)
                }
                Def::Symbol(symbol) => {
                    for (modifiers, value, _) in symbol.variants() {
                        let path = match modifiers.as_str() {
                            "" => format!("{prefix}{name}"),
                            modifiers => format!("{prefix}{name}.{modifiers}"),
                        };
                        f(&path, value);
                    }
                }
            }
        }
    }

    #[test]
    fn symbols() {
        assert_eq!(math_symbol("alpha"), Some("α"));
        assert_eq!(math_symbol("arrow.r.long"), Some("⟶"));
        assert_eq!(math_symbol("arrow.long.r"), Some("⟶"));
        assert_eq!(math_symbol("dot"), Some("⋅"));
        assert_eq!(math_symbol("gender.female.double"), Some("⚢"));
        for path in [
            "alpha.foo",
            "alpha.",
            ".alpha",
            "arrow..r",
            "gender",
            "sym.alpha",
            "dif",
        ] {
            assert_eq!(math_symbol(path), None, "{path}");
        }

        assert_eq!(code_symbol("sym.qed"), Some("∎"));
        assert_eq!(code_symbol("#sym.arrow.r"), Some("→"));
        assert_eq!(code_symbol("#emoji.face"), Some("😀"));
        for code in ["alpha", "#alpha", "sym", "calc.pi", "##sym.qed"] {
            assert_eq!(code_symbol(code), None, "{code}");
        }

        assert_eq!(shorthand("->"), Some("→"));
        assert_eq!(shorthand("---"), Some("—"));
        assert_eq!(shorthand("-"), None);
    }

    #[test]
    fn every_symbol_can_be_concealed() {
        for (module, name) in [(codex::SYM, "sym"), (codex::EMOJI, "emoji")] {
            for_each_variant(module, "", &mut |path, value| {
                let code = format!("#{name}.{path}");
                assert_eq!(code_symbol(&code), Some(value), "{code}");
                if name == "sym" {
                    assert_eq!(math_symbol(path), Some(value), "{path}");
                }
                assert!(code.len() <= MAX_CONCEALED_BYTES, "{code}");
                // a visible symbol is drawn as a single grapheme
                if is_visible(value) {
                    assert_eq!(value.graphemes(true).count(), 1, "{code}: {value:?}");
                }
            });
        }
    }
}
