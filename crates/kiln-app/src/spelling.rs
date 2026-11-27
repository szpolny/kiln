//! Offline spelling checks for prose in the composer.

pub(crate) mod download;

use encoding_rs::{Encoding, UTF_8};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use spellbook::Dictionary;
use std::{
    ops::Range,
    sync::{Arc, LazyLock},
};
use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory};
use unicode_segmentation::UnicodeSegmentation;

static DICTIONARY: LazyLock<Dictionary> = LazyLock::new(|| {
    Dictionary::new(
        include_str!("../assets/dictionaries/en_US.aff"),
        include_str!("../assets/dictionaries/en_US.dic"),
    )
    .expect("bundled spelling dictionary must parse")
});

#[derive(Clone, Default)]
pub struct Dictionaries {
    english: bool,
    additional: Arc<[Arc<Dictionary>]>,
}

impl Dictionaries {
    pub fn new(english: bool, additional: Vec<Arc<Dictionary>>) -> Self {
        Self {
            english,
            additional: additional.into(),
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.english && self.additional.is_empty()
    }

    fn iter(&self) -> impl Iterator<Item = &Dictionary> {
        self.additional
            .iter()
            .map(Arc::as_ref)
            .chain(self.english.then(|| &*DICTIONARY))
    }

    pub fn suggestions(&self, word: &str) -> Vec<String> {
        const LIMIT: usize = 5;
        let candidates: Vec<_> = self
            .iter()
            .map(|dictionary| {
                let mut choices = Vec::new();
                dictionary.suggest(word, &mut choices);
                choices
            })
            .collect();

        // Give each language a chance to offer its highest-ranked suggestions.
        let mut suggestions = Vec::new();
        let ranks = candidates.iter().map(Vec::len).max().unwrap_or(0);
        for rank in 0..ranks {
            for word in candidates.iter().filter_map(|choices| choices.get(rank)) {
                if !suggestions.contains(word) {
                    suggestions.push(word.clone());
                    if suggestions.len() == LIMIT {
                        return suggestions;
                    }
                }
            }
        }
        suggestions
    }

    pub fn misspellings(&self, text: &str) -> Vec<Range<usize>> {
        if self.is_empty() {
            return Vec::new();
        }
        let dictionaries: Vec<_> = self.iter().collect();
        misspellings(text, |word| {
            dictionaries.iter().any(|dict| dict.check(word))
        })
    }
}

/// Decode both Hunspell files using the encoding declared in the .aff file.
pub fn parse_dictionary(aff: &[u8], dic: &[u8]) -> Result<Arc<Dictionary>, String> {
    let label = aff
        .strip_prefix(b"\xef\xbb\xbf")
        .unwrap_or(aff)
        .split(|&byte| byte == b'\n')
        .find_map(|line| {
            let mut fields = line
                .split(|byte| byte.is_ascii_whitespace())
                .filter(|field| !field.is_empty());
            if fields.next()? == b"SET" {
                fields.next()
            } else {
                None
            }
        });
    let encoding = match label {
        Some(label) => {
            let name = String::from_utf8_lossy(label).to_ascii_lowercase();
            let alias = name
                .strip_prefix("microsoft-cp")
                .map(|page| format!("windows-{page}"));
            Encoding::for_label_no_replacement(alias.as_deref().unwrap_or(&name).as_bytes())
                .ok_or_else(|| format!("Unsupported dictionary encoding {name}."))?
        }
        None => UTF_8,
    };
    let aff = encoding
        .decode_without_bom_handling_and_without_replacement(aff)
        .ok_or("The .aff file does not match its declared encoding.")?;
    let dic = encoding
        .decode_without_bom_handling_and_without_replacement(dic)
        .ok_or("The .dic file does not match the encoding declared in its .aff file.")?;
    let aff = aff.strip_prefix('\u{feff}').unwrap_or(&aff);
    let dic = dic.strip_prefix('\u{feff}').unwrap_or(&dic);
    Dictionary::new(aff, dic)
        .map(Arc::new)
        .map_err(|e| format!("Could not load dictionary. {e}"))
}

fn misspellings(text: &str, is_correct: impl Fn(&str) -> bool) -> Vec<Range<usize>> {
    let mut misspelled = Vec::new();
    let mut in_code_block = false;
    for (event, source) in Parser::new(text).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => in_code_block = true,
            Event::End(TagEnd::CodeBlock) => in_code_block = false,
            Event::Text(_) if !in_code_block => {
                let mut start = source.start;
                for token in text[source].split_inclusive(char::is_whitespace) {
                    let end = start + token.len();
                    // URLs, paths, file references, and snake_case are common in prompts.
                    if !token.contains(['/', '\\', '@', '_']) {
                        for (offset, word) in token.unicode_word_indices() {
                            if word.len() <= spellbook::MAX_WORD_LEN
                                && (!word.chars().any(char::is_uppercase)
                                    || word.chars().any(char::is_lowercase))
                                && word.chars().all(|c| {
                                    c.is_alphabetic()
                                        || matches!(c, '\'' | '’')
                                        || c.general_category_group() == GeneralCategoryGroup::Mark
                                })
                                && !is_correct(word)
                            {
                                let from = start + offset;
                                misspelled.push(from..from + word.len());
                            }
                        }
                    }
                    start = end;
                }
            }
            _ => {}
        }
    }
    misspelled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_prose_with_utf8_offsets_and_skips_code_and_paths() {
        let text =
            "🙂 Teh copy isn’t writen. `writen` foo_bar src/writen.rs @writen\n\n```\nwriten\n```";
        let found = Dictionaries::new(true, vec![]).misspellings(text);
        assert_eq!(
            found
                .into_iter()
                .map(|range| &text[range])
                .collect::<Vec<_>>(),
            ["Teh", "writen"]
        );
    }

    #[test]
    fn suggests_corrections_with_matching_case() {
        let dictionaries = Dictionaries::new(true, vec![]);
        let words = dictionaries.suggestions("Writen");
        assert!(words.iter().any(|word| word == "Written"), "{words:?}");
        let words = dictionaries.suggestions("recieve");
        assert!(words.iter().any(|word| word == "receive"), "{words:?}");
    }

    #[test]
    fn checks_words_and_offers_corrections_in_an_added_language() {
        let polish = parse_dictionary(
            b"SET ISO8859-2\nTRY abcdefghijklmnopqrstuvwxyz\xb3\n",
            b"1\nprzyk\xb3adowy\n",
        )
        .unwrap();
        let dictionaries = Dictionaries::new(true, vec![polish]);
        let draft = "This is przykładowy and writen.";
        let mistakes = dictionaries.misspellings(draft);
        assert_eq!(
            mistakes.into_iter().map(|r| &draft[r]).collect::<Vec<_>>(),
            ["writen"]
        );
        assert!(dictionaries
            .suggestions("przykladowy")
            .iter()
            .any(|word| word == "przykładowy"));
    }

    #[test]
    fn checks_languages_with_uncased_letters_and_combining_marks() {
        let language = parse_dictionary(b"SET UTF-8\n", "1\nहिन्दी\n".as_bytes()).unwrap();
        let dictionaries = Dictionaries::new(false, vec![language]);
        let draft = "हिन्दी हहिन्दी";
        assert_eq!(
            dictionaries.misspellings(draft),
            vec!["हिन्दी ".len()..draft.len()]
        );
    }
}
