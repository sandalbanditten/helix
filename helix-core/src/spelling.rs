//! Natural languages for spell checking.

use std::{fmt, str::FromStr};

use ropey::RopeSlice;
use smartstring::{LazyCompact, SmartString};
use whatlang::{Detector, Lang};

use crate::syntax::{Loader, Syntax};

/// A spelling dictionary identifier, such as `en_US`, naming the dictionary files
/// `dictionaries/<id>/<id>.{aff,dic}` in the runtime directories.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpellingLanguage(SmartString<LazyCompact>);

impl SpellingLanguage {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The language of the dictionary, from the ISO 639 code its name starts with.
    fn lang(&self) -> Option<Lang> {
        let code = self.0.split(['_', '-']).next()?;
        if code.len() == 2 {
            ISO_639_1
                .iter()
                .find(|(iso_639_1, _)| *iso_639_1 == code)
                .map(|&(_, lang)| lang)
        } else {
            Lang::from_code(code)
        }
    }
}

impl fmt::Display for SpellingLanguage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug)]
pub struct ParseSpellingLanguageError(String);

impl FromStr for SpellingLanguage {
    type Err = ParseSpellingLanguageError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // The identifier is interpolated into a dictionary file path, so restrict it to a single
        // safe path component: non-empty ASCII alphanumerics plus `_` and `-`.
        if !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            Ok(Self(s.into()))
        } else {
            Err(ParseSpellingLanguageError(s.to_owned()))
        }
    }
}

impl fmt::Display for ParseSpellingLanguageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid spelling language '{}': expected a dictionary name of ASCII letters, digits, '_' or '-'",
            self.0
        )
    }
}

impl std::error::Error for ParseSpellingLanguageError {}

impl serde::Serialize for SpellingLanguage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for SpellingLanguage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let string = <String as serde::Deserialize>::deserialize(deserializer)?;
        string.parse().map_err(serde::de::Error::custom)
    }
}

/// The chars at the start of a document whose spell checked text is sampled to detect its language.
const DETECTION_SCAN_CHARS: usize = 16 * 1024;
/// The bytes of spell checked text sampled to detect a document's language.
const DETECTION_SAMPLE_BYTES: usize = 4096;

/// Detects which of the `candidates` a document's prose is written in, from its start. Returns
/// all candidates when the language can't be told.
pub fn detect_language(
    candidates: &[SpellingLanguage],
    text: RopeSlice,
    syntax: Option<&Syntax>,
    loader: &Loader,
) -> Vec<SpellingLanguage> {
    let mut langs = Vec::new();
    for lang in candidates.iter().filter_map(SpellingLanguage::lang) {
        if !langs.contains(&lang) {
            langs.push(lang);
        }
    }
    if langs.len() < 2 {
        return candidates.to_vec();
    }
    match Detector::with_allowlist(langs).detect(&detection_sample(text, syntax, loader)) {
        Some(info) if info.is_reliable() => candidates
            .iter()
            .filter(|candidate| candidate.lang().is_none_or(|lang| lang == info.lang()))
            .cloned()
            .collect(),
        _ => candidates.to_vec(),
    }
}

/// The spell checked text at the start of a document.
fn detection_sample(text: RopeSlice, syntax: Option<&Syntax>, loader: &Loader) -> String {
    let start = 0..text.char_to_byte(text.len_chars().min(DETECTION_SCAN_CHARS));
    let regions = match syntax {
        Some(syntax) => syntax.spell_regions(text, loader, start),
        // Without a syntax tree, all of the text is checked.
        None => vec![start],
    };
    let mut sample = String::new();
    for region in regions {
        for chunk in text.byte_slice(region).chunks() {
            if sample.len() >= DETECTION_SAMPLE_BYTES {
                return sample;
            }
            sample.push_str(chunk);
        }
        sample.push(' ');
    }
    sample
}

/// The ISO 639-1 codes of the languages detection knows, which name most dictionaries.
const ISO_639_1: &[(&str, Lang)] = &[
    ("af", Lang::Afr),
    ("ak", Lang::Aka),
    ("am", Lang::Amh),
    ("ar", Lang::Ara),
    ("az", Lang::Aze),
    ("be", Lang::Bel),
    ("bg", Lang::Bul),
    ("bn", Lang::Ben),
    ("ca", Lang::Cat),
    ("cs", Lang::Ces),
    ("cy", Lang::Cym),
    ("da", Lang::Dan),
    ("de", Lang::Deu),
    ("el", Lang::Ell),
    ("en", Lang::Eng),
    ("eo", Lang::Epo),
    ("es", Lang::Spa),
    ("et", Lang::Est),
    ("fa", Lang::Pes),
    ("fi", Lang::Fin),
    ("fr", Lang::Fra),
    ("gu", Lang::Guj),
    ("he", Lang::Heb),
    ("hi", Lang::Hin),
    ("hr", Lang::Hrv),
    ("hu", Lang::Hun),
    ("hy", Lang::Hye),
    ("id", Lang::Ind),
    ("it", Lang::Ita),
    ("ja", Lang::Jpn),
    ("jv", Lang::Jav),
    ("ka", Lang::Kat),
    ("km", Lang::Khm),
    ("kn", Lang::Kan),
    ("ko", Lang::Kor),
    ("la", Lang::Lat),
    ("lt", Lang::Lit),
    ("lv", Lang::Lav),
    ("mk", Lang::Mkd),
    ("ml", Lang::Mal),
    ("mr", Lang::Mar),
    ("my", Lang::Mya),
    ("nb", Lang::Nob),
    ("ne", Lang::Nep),
    ("nl", Lang::Nld),
    ("or", Lang::Ori),
    ("pa", Lang::Pan),
    ("pl", Lang::Pol),
    ("pt", Lang::Por),
    ("ro", Lang::Ron),
    ("ru", Lang::Rus),
    ("si", Lang::Sin),
    ("sk", Lang::Slk),
    ("sl", Lang::Slv),
    ("sn", Lang::Sna),
    ("sr", Lang::Srp),
    ("sv", Lang::Swe),
    ("ta", Lang::Tam),
    ("te", Lang::Tel),
    ("th", Lang::Tha),
    ("tk", Lang::Tuk),
    ("tl", Lang::Tgl),
    ("tr", Lang::Tur),
    ("uk", Lang::Ukr),
    ("ur", Lang::Urd),
    ("uz", Lang::Uzb),
    ("vi", Lang::Vie),
    ("yi", Lang::Yid),
    ("zh", Lang::Cmn),
    ("zu", Lang::Zul),
];

#[cfg(test)]
mod test {
    use once_cell::sync::Lazy;

    use super::*;
    use crate::Rope;

    static LOADER: Lazy<Loader> = Lazy::new(crate::config::default_lang_loader);

    fn detect(candidates: &[&str], language: Option<&str>, source: &str) -> Vec<String> {
        let candidates: Vec<SpellingLanguage> = candidates
            .iter()
            .map(|name| name.parse().unwrap())
            .collect();
        let text = Rope::from_str(source);
        let syntax = language.map(|language| {
            let language = LOADER.language_for_name(language).unwrap();
            Syntax::new(text.slice(..), language, &LOADER).unwrap()
        });
        detect_language(&candidates, text.slice(..), syntax.as_ref(), &LOADER)
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    const DANISH: &str = "Det var en rigtig god dag i sommerhuset, hvor børnene legede med \
        fodbolden, og vi spiste smørrebrød i haven, mens solen skinnede over marken.";
    const ENGLISH: &str = "It was a really good day at the summer house, where the children \
        played with the ball and we ate sandwiches in the garden while the sun was shining.";

    #[test]
    fn detects_the_language_among_the_candidates() {
        let candidates = ["en_US", "da_DK"];
        assert_eq!(detect(&candidates, None, DANISH), ["da_DK"]);
        assert_eq!(detect(&candidates, None, ENGLISH), ["en_US"]);
        // Dictionaries of the same language are all used.
        assert_eq!(
            detect(&["en_US", "da_DK", "en_GB"], None, ENGLISH),
            ["en_US", "en_GB"]
        );
    }

    #[test]
    fn uses_all_candidates_when_unsure() {
        let candidates = ["en_US", "da_DK"];
        assert_eq!(detect(&candidates, None, "ok"), candidates);
        // Nothing to choose between.
        assert_eq!(
            detect(&["en_US", "en_GB"], None, DANISH),
            ["en_US", "en_GB"]
        );
        // Languages detection doesn't know are always used.
        assert_eq!(
            detect(&["en_US", "da_DK", "fo_FO"], None, DANISH),
            ["da_DK", "fo_FO"]
        );
    }

    #[test]
    fn detects_from_the_spell_checked_text() {
        // The Danish prose decides, not the English comments of the code block before it.
        let source = format!(
            "```rust\n{}```\n\n{DANISH}\n",
            "// the summer house is where we play with the ball\n".repeat(20)
        );
        assert_eq!(
            detect(&["en_US", "da_DK"], Some("markdown"), &source),
            ["da_DK"]
        );
    }

    #[test]
    fn dictionary_names_start_with_their_language() {
        let lang = |name: &str| name.parse::<SpellingLanguage>().unwrap().lang();
        assert_eq!(lang("da_DK"), Some(Lang::Dan));
        assert_eq!(lang("de_DE_frami"), Some(Lang::Deu));
        assert_eq!(lang("ca"), Some(Lang::Cat));
        assert_eq!(lang("en-GB"), Some(Lang::Eng));
        assert_eq!(lang("nn_NO"), None);
    }
}
