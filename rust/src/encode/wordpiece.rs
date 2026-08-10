//! Text -> token ids, the way `tokenizer.json` says.
//!
//! The C++ tree spent 150 lines of `unicode.hpp` hand-decoding UTF-8 into a
//! `vector<uint32_t>` before it could look at a character, and another 164 in
//! `wordpiece.cpp` walking byte offsets while dodging continuation bytes. Neither
//! survives: `str` is UTF-8 by construction, `chars()` is the decoder, and
//! `is_char_boundary` is the guard. What is left is the part that is a
//! *specification* rather than plumbing — BertNormalizer's exact fold, BERT's
//! punctuation set, and greedy longest-match — plus one table std cannot supply.

use std::collections::HashMap;
use std::path::Path;

use crate::R;

/// Accent folding for Latin-1 Supplement, lowercase half (U+00E0..U+00FF).
///
/// `char::to_lowercase` covers case, but std has no NFD, so this is the one piece
/// of Unicode data that has to be carried. Each entry is the base letter, or NUL
/// where the codepoint has none and should pass through untouched.
const LATIN1: &[u8; 32] = b"aaaaaaaceeeeiiii\x00nooooo\x00ouuuuy\x00y";

/// Same, for Latin Extended-A (U+0100..U+017F).
///
/// Generated from NFD rather than typed by hand — the C++ table this replaces had
/// drifted two positions out of alignment past U+0150, so `ś` folded to `r` and
/// every Polish word containing it tokenised to a different token than intended.
/// Ligatures and barred letters (ł, đ, ħ, ı, ŋ, ŧ, œ) have no decomposition at all
/// and are folded to their first base letter on purpose: an English WordPiece
/// vocabulary has `l` and does not have `ł`, and [UNK] is dropped by the pooler.
const LATIN_A: &[u8; 128] = b"aaaaaaccccccccddddeeeeeeeeeegggggggghhhhiiiiiiiiiiiijjkkkllllll\x00\x00llnnnnnn\x00nnoooooooorrrrrrssssssssttttttuuuuuuuuuuuuwwyyyzzzzzzs";

fn strip_accent(c: char) -> char {
    let base = match c as u32 {
        cp @ 0xE0..=0xFF => LATIN1[cp as usize - 0xE0],
        cp @ 0x100..=0x17F => LATIN_A[cp as usize - 0x100],
        // Cyrillic precomposed letters. Not optional: the vocab has и and е but
        // not й or ё, so leaving these composed turns every Russian word that
        // contains one into [UNK], which the pooler then discards — quietly
        // degrading the vector rather than failing.
        0x439 => return '\u{438}', // й -> и
        0x451 | 0x450 => return '\u{435}', // ё, ѐ -> е
        0x45D => return '\u{438}', // ѝ -> и
        0x45E => return '\u{443}', // ў -> у
        0x453 => return '\u{433}', // ѓ -> г
        0x45C => return '\u{43A}', // ќ -> к
        _ => 0,
    };
    if base == 0 { c } else { base as char }
}

/// BERT's definition: ASCII punctuation plus the Unicode punctuation blocks.
/// Not `char::is_ascii_punctuation` — that misses the blocks, and BERT's set
/// deliberately includes symbols like `$` and `^` that Unicode files under S*.
fn is_punct(c: char) -> bool {
    let cp = c as u32;
    matches!(cp, 33..=47 | 58..=64 | 91..=96 | 123..=126)
        || matches!(cp, 0x2010..=0x2027 | 0x2030..=0x205E | 0x3001..=0x303F | 0xFF01..=0xFF0F)
        || (matches!(cp, 0xA1..=0xBF) && !matches!(cp, 0xAA | 0xB5 | 0xBA))
}

/// Han and its extensions. Each ideograph becomes its own word, which is what
/// `handle_chinese_chars` means.
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF
        | 0x20000..=0x2A6DF | 0x2A700..=0x2B73F | 0x2B740..=0x2B81F
        | 0x2B820..=0x2CEAF | 0x2F800..=0x2FA1F)
}

/// BertNormalizer with `clean_text`, `handle_chinese_chars` and `lowercase` on and
/// `strip_accents` null — which HuggingFace resolves to the value of `lowercase`.
///
/// Order matters and is not the obvious one: `\t\n\r` are controls to std but
/// whitespace to BERT, while U+000B and U+000C are whitespace to std and controls
/// to BERT. Getting that backwards changes tokenisation on every transcript that
/// contains a form feed.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    for c in text.chars() {
        if c == '\u{FFFD}' {
            continue;
        }
        if c == '\t' || c == '\n' || c == '\r' {
            out.push(' ');
        } else if c.is_control() {
            continue; // clean_text; NUL lands here too
        } else if c.is_whitespace() {
            out.push(' ');
        } else if is_cjk(c) {
            out.push(' ');
            out.push(c);
            out.push(' ');
        } else {
            for lowered in c.to_lowercase() {
                out.push(strip_accent(lowered));
            }
        }
    }
    out
}

pub struct WordPiece {
    vocab: HashMap<String, u32>,
    /// `##`, prepended to every piece after the first in a word.
    prefix: String,
    max_chars_per_word: usize,
    unk: u32,
    /// Median vocabulary token length in bytes. model2vec pre-truncates text to
    /// `max_tokens * this` characters before tokenising, so a megabyte of
    /// transcript is not tokenised in full only to keep its first 512 tokens.
    pub median_token_bytes: usize,
}

impl WordPiece {
    pub fn load(path: &Path) -> R<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let doc: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| format!("unparsable tokenizer.json at {}: {e}", path.display()))?;
        let model = doc.get("model").ok_or("tokenizer.json has no model section")?;

        let table = model
            .get("vocab")
            .and_then(|v| v.as_object())
            .ok_or("tokenizer.json has no vocab")?;
        let mut vocab = HashMap::with_capacity(table.len());
        let mut lens = Vec::with_capacity(table.len());
        for (token, id) in table {
            let Some(id) = id.as_u64() else { continue };
            lens.push(token.len());
            vocab.insert(token.clone(), id as u32);
        }
        if vocab.is_empty() {
            return Err("tokenizer vocab is empty".into());
        }
        lens.sort_unstable();
        let median_token_bytes = lens[lens.len() / 2].max(1);

        let unk_token = model.get("unk_token").and_then(|v| v.as_str()).unwrap_or("[UNK]");
        let unk = *vocab.get(unk_token).ok_or_else(|| {
            format!("tokenizer claims unk_token='{unk_token}' but it is not in the vocab")
        })?;

        Ok(Self {
            vocab,
            prefix: model
                .get("continuing_subword_prefix")
                .and_then(|v| v.as_str())
                .unwrap_or("##")
                .to_string(),
            max_chars_per_word: model
                .get("max_input_chars_per_word")
                .and_then(|v| v.as_u64())
                .map_or(100, |n| n as usize),
            unk,
            median_token_bytes,
        })
    }

    pub fn unk(&self) -> u32 {
        self.unk
    }

    /// Greedy longest-match-first, the WordPiece algorithm. A word with no valid
    /// split contributes a single [UNK], never a partial decomposition — hence the
    /// rollback to `mark`.
    ///
    /// `buf` is the caller's scratch: at ~55 vocabulary probes per word and 57,683
    /// rows to sweep, a fresh `String` per probe is the whole cost of the pass.
    fn push_word(&self, word: &str, ids: &mut Vec<u32>, buf: &mut String) {
        if word.is_empty() {
            return;
        }
        if word.chars().count() > self.max_chars_per_word {
            ids.push(self.unk);
            return;
        }
        let mark = ids.len();
        let mut start = 0;
        while start < word.len() {
            let mut end = word.len();
            let mut hit = false;
            while end > start {
                // Never split inside a character. The C++ did this by inspecting
                // continuation bits; std already knows.
                if !word.is_char_boundary(end) {
                    end -= 1;
                    continue;
                }
                buf.clear();
                if start > 0 {
                    buf.push_str(&self.prefix);
                }
                buf.push_str(&word[start..end]);
                if let Some(&id) = self.vocab.get(buf.as_str()) {
                    ids.push(id);
                    start = end;
                    hit = true;
                    break;
                }
                end -= 1;
            }
            if !hit {
                ids.truncate(mark);
                ids.push(self.unk);
                return;
            }
        }
    }

    /// Normalize, split on whitespace and punctuation (BertPreTokenizer), tokenise.
    ///
    /// Fused into one pass rather than the C++'s `Vec<String>` of words: at ~300
    /// words per row that intermediate was 17 million allocations across a full
    /// sweep, and it never outlived the loop that built it.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let norm = normalize(text);
        let mut ids = Vec::new();
        let mut word = String::new();
        let mut buf = String::new();
        for c in norm.chars() {
            if c.is_whitespace() {
                self.push_word(&word, &mut ids, &mut buf);
                word.clear();
            } else if is_punct(c) {
                self.push_word(&word, &mut ids, &mut buf);
                word.clear();
                word.push(c); // each punctuation mark is its own word
                self.push_word(&word, &mut ids, &mut buf);
                word.clear();
            } else {
                word.push(c);
            }
        }
        self.push_word(&word, &mut ids, &mut buf);
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizer_folds_accents_and_case() {
        assert_eq!(normalize("Café ÉLAN"), "cafe elan");
        // The exact defect the regenerated table fixes: `ś` used to fold to `r`.
        assert_eq!(normalize("Śniadanie"), "sniadanie");
        assert_eq!(normalize("ŽluŤ ŁÓDŹ"), "zlut lodz");
    }

    #[test]
    fn normalizer_treats_tabs_as_space_and_form_feed_as_control() {
        assert_eq!(normalize("a\tb\nc"), "a b c");
        // U+000B is whitespace to std and a control to BERT: it must vanish, not
        // become a space, or every tokenisation downstream of it shifts.
        assert_eq!(normalize("a\u{0B}b"), "ab");
        assert_eq!(normalize("a\u{00A0}b"), "a b"); // NBSP is space, not control
    }

    #[test]
    fn cjk_is_isolated_per_ideograph() {
        assert_eq!(normalize("中文"), " 中  文 ");
    }

    #[test]
    fn punctuation_matches_berts_set_not_asciis() {
        assert!(is_punct('$') && is_punct('^') && is_punct('—') && is_punct('，'));
        assert!(!is_punct('a') && !is_punct(' ') && !is_punct('µ'));
    }
}
