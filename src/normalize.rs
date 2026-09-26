//! WYSIWYG input policy — the single source of truth for what the editor keeps,
//! drops, or folds as text enters the buffer (typing, paste, or file load).
//!
//! Two classes of non-ASCII input are neutralized so that what you see is what
//! you get:
//!
//!   * **Invisible / zero-width / formatting** characters are **dropped**. They
//!     can't be seen, yet they can hide text, split identifiers, or reorder a
//!     line (bidi overrides). A character you can't see shouldn't be in the
//!     buffer.
//!
//!   * **Visible "confusables"** that imitate an ASCII character are **folded**
//!     to the ASCII character they look like: any Unicode space becomes a plain
//!     space, curly quotes become straight quotes, Unicode dashes become `-`.
//!     These are exactly the substitutions word processors and web pages make
//!     automatically — in prose they're invisible niceties, but in code or SQL
//!     they silently break things while looking perfectly correct (a
//!     non-breaking space that isn't a space, a curly quote a parser rejects).
//!
//!   * **Homoglyphs inside mixed-script words** are **folded** to Latin. A
//!     Cyrillic or Greek letter that is indistinguishable from a Latin one
//!     (Cyrillic `а`, Greek `Ο`) is only a spoof when it sits inside an
//!     otherwise Latin word, like `password` spelled with a Cyrillic `а`
//!     (U+0430). So this is judged per word, not
//!     per character: a word mixing Latin letters with look-alikes is folded
//!     to all-Latin, while a word written wholly in Cyrillic or Greek (real
//!     text, even inside a string literal) is left alone. A word holding any
//!     Cyrillic or Greek letter that has no Latin twin (`д`, `λ`) is clearly
//!     meant as that script and is also left alone. Because it needs the
//!     whole word, this runs on paste and file load, not per keystroke.
//!
//! Every path that ingests text routes through here, so the policy can't drift
//! between typing, pasting, and opening a file. Deliberately conservative: only
//! characters that are near-indistinguishable from an ASCII glyph are folded.
//! Distinct typography (ellipsis, guillemets) and legitimate letters (accented
//! Latin, CJK, Cyrillic, the ʻokina modifier letter, measurement primes) are
//! left untouched.

/// The policy's decision for a single character.
pub enum Fold {
    /// Keep the character unchanged.
    Keep,
    /// Drop it entirely (invisible / zero-width / formatting).
    Drop,
    /// Replace it with an ASCII string (a tab's spaces, or a confusable folded
    /// to the ASCII character it imitates).
    Replace(&'static str),
}

/// Classify a single character under the WYSIWYG input policy.
pub fn fold_char(c: char) -> Fold {
    match c {
        // Tab -> 4 spaces (editor convention; sage does not store hard tabs).
        '\t' => Fold::Replace("    "),
        // Carriage returns are dropped: CRLF collapses to LF, a lone CR is
        // removed. (Kept as a drop so no separate \r\n pass is needed.)
        '\r' => Fold::Drop,

        // ---- Invisible / zero-width / formatting: drop ----
        '\u{200B}' | // zero-width space
        '\u{200C}' | // zero-width non-joiner
        '\u{200D}' | // zero-width joiner
        '\u{200E}' | // left-to-right mark
        '\u{200F}' | // right-to-left mark
        '\u{202A}'..='\u{202E}' | // bidi embedding / override
        '\u{2060}'..='\u{2064}' | // word joiner / invisible operators
        '\u{2066}'..='\u{206F}' | // bidi isolates / deprecated format chars
        '\u{FEFF}' | // zero-width no-break space (BOM)
        '\u{FFF9}'..='\u{FFFB}' | // interlinear annotation
        '\u{00AD}' | // soft hyphen
        '\u{034F}' | // combining grapheme joiner
        '\u{061C}' | // Arabic letter mark
        '\u{115F}' | '\u{1160}' | // Hangul fillers
        '\u{17B4}' | '\u{17B5}' | // Khmer inherent vowels
        '\u{180E}' | // Mongolian vowel separator
        '\u{3164}' | // Hangul filler
        '\u{FFA0}' | // halfwidth Hangul filler
        '\u{FE00}'..='\u{FE0F}' | // variation selectors
        '\u{E0100}'..='\u{E01EF}' => Fold::Drop, // variation selectors supplement

        // ---- Visible confusables: fold to the ASCII they imitate ----

        // Spaces that render like a normal space.
        '\u{00A0}' | // no-break space
        '\u{1680}' | // ogham space mark
        '\u{2000}'..='\u{200A}' | // en quad .. hair space
        '\u{202F}' | // narrow no-break space
        '\u{205F}' | // medium mathematical space
        '\u{3000}' => Fold::Replace(" "), // ideographic space

        // Curly / "smart" single quotes and apostrophes.
        '\u{2018}' | // left single quotation mark
        '\u{2019}' | // right single quotation mark (curly apostrophe)
        '\u{201A}' | // single low-9 quotation mark
        '\u{201B}' => Fold::Replace("'"), // single high-reversed-9

        // Curly / "smart" double quotes.
        '\u{201C}' | // left double quotation mark
        '\u{201D}' | // right double quotation mark
        '\u{201E}' | // double low-9 quotation mark
        '\u{201F}' => Fold::Replace("\""), // double high-reversed-9

        // Hyphens, dashes, and the minus sign that read as an ASCII hyphen.
        '\u{2010}' | // hyphen
        '\u{2011}' | // non-breaking hyphen
        '\u{2012}' | // figure dash
        '\u{2013}' | // en dash
        '\u{2014}' | // em dash
        '\u{2015}' | // horizontal bar
        '\u{2212}' | // minus sign
        '\u{FE58}' | // small em dash
        '\u{FE63}' | // small hyphen-minus
        '\u{FF0D}' => Fold::Replace("-"), // fullwidth hyphen-minus

        // Everything else — including legitimate non-ASCII letters — is kept.
        _ => Fold::Keep,
    }
}

/// Apply the WYSIWYG input policy to a whole string (paste / file load).
pub fn normalize_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match fold_char(c) {
            Fold::Keep => out.push(c),
            Fold::Drop => {}
            Fold::Replace(s) => out.push_str(s),
        }
    }
    fold_mixed_script_words(out)
}

/// The Latin letter a Cyrillic or Greek letter is visually indistinguishable
/// from, if it has one. Deliberately tight: only letters that render the same
/// as their Latin twin in ordinary fonts. Greek `α` and `μ`, for instance, are
/// distinct shapes (and `μs` must never become `us`), so they are not here.
fn latin_twin(c: char) -> Option<char> {
    Some(match c {
        // Cyrillic capitals
        'А' => 'A', 'В' => 'B', 'Е' => 'E', 'К' => 'K', 'М' => 'M', 'Н' => 'H',
        'О' => 'O', 'Р' => 'P', 'С' => 'C', 'Т' => 'T', 'Х' => 'X', 'У' => 'Y',
        'Ѕ' => 'S', 'І' => 'I', 'Ј' => 'J',
        // Cyrillic small
        'а' => 'a', 'е' => 'e', 'о' => 'o', 'р' => 'p', 'с' => 'c', 'х' => 'x',
        'у' => 'y', 'ѕ' => 's', 'і' => 'i', 'ј' => 'j', 'һ' => 'h', 'ԁ' => 'd',
        // Greek capitals
        'Α' => 'A', 'Β' => 'B', 'Ε' => 'E', 'Ζ' => 'Z', 'Η' => 'H', 'Ι' => 'I',
        'Κ' => 'K', 'Μ' => 'M', 'Ν' => 'N', 'Ο' => 'O', 'Ρ' => 'P', 'Τ' => 'T',
        'Υ' => 'Y', 'Χ' => 'X',
        // Greek small
        'ο' => 'o', 'ν' => 'v',
        _ => return None,
    })
}

/// A letter of the Latin script: ASCII plus the Latin-1, Extended-A/B and
/// Extended Additional letter blocks (so `café` counts as Latin).
fn is_latin_letter(c: char) -> bool {
    c.is_ascii_alphabetic()
        || (matches!(c, '\u{00C0}'..='\u{024F}' | '\u{1E00}'..='\u{1EFF}')
            && c != '\u{00D7}' // multiplication sign
            && c != '\u{00F7}') // division sign
}

/// A letter of the Cyrillic or Greek script.
fn is_cyrillic_or_greek(c: char) -> bool {
    matches!(c, '\u{0370}'..='\u{03FF}' | '\u{1F00}'..='\u{1FFF}' | '\u{0400}'..='\u{052F}')
        && c.is_alphabetic()
}

/// Fold homoglyphs in mixed-script words to Latin (see the module docs).
///
/// A word is a run of alphanumerics and underscores, so an identifier like
/// `user_pass` is judged as one word. A word is folded only when it holds at
/// least one Latin letter and every Cyrillic or Greek letter in it has a Latin
/// twin. Returns the input untouched (no copy) when nothing needs folding,
/// which keeps loading large files cheap.
fn fold_mixed_script_words(text: String) -> String {
    // Fast path: without a single look-alike there is nothing to fold.
    if text.is_ascii() || !text.chars().any(|c| latin_twin(c).is_some()) {
        return text;
    }

    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while !rest.is_empty() {
        // Copy the non-word run through unchanged.
        let start = rest.find(|c: char| is_word(c)).unwrap_or(rest.len());
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        // Take the next word and decide whether it is a spoof.
        let end = rest.find(|c: char| !is_word(c)).unwrap_or(rest.len());
        let word = &rest[..end];
        let has_latin = word.chars().any(is_latin_letter);
        let foreign_all_twins = word
            .chars()
            .filter(|&c| is_cyrillic_or_greek(c))
            .all(|c| latin_twin(c).is_some());
        let has_foreign = word.chars().any(is_cyrillic_or_greek);
        if has_latin && has_foreign && foreign_all_twins {
            out.extend(word.chars().map(|c| latin_twin(c).unwrap_or(c)));
        } else {
            out.push_str(word);
        }
        rest = &rest[end..];
    }
    out
}

/// Apply the policy to a single typed character, returning the text to insert
/// (empty string if the character is dropped).
pub fn fold_char_to_str(c: char) -> String {
    match fold_char(c) {
        Fold::Keep => c.to_string(),
        Fold::Drop => String::new(),
        Fold::Replace(s) => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_confusable_space_quotes_and_dashes() {
        // NBSP -> space, curly quotes -> straight, en/em dash -> '-'.
        assert_eq!(normalize_text("a\u{00A0}b"), "a b");
        assert_eq!(normalize_text("\u{2018}x\u{2019}"), "'x'");
        assert_eq!(normalize_text("\u{201C}x\u{201D}"), "\"x\"");
        assert_eq!(normalize_text("a\u{2013}b\u{2014}c"), "a-b-c");
        // A realistic Word-mangled SQL literal becomes valid ASCII.
        assert_eq!(
            normalize_text("WHERE name = \u{2018}O\u{2019}Hara\u{2019}"),
            "WHERE name = 'O'Hara'"
        );
    }

    #[test]
    fn drops_invisibles_and_converts_crlf_and_tabs() {
        assert_eq!(normalize_text("a\u{200B}b"), "ab"); // zero-width space
        assert_eq!(normalize_text("\u{FEFF}hi"), "hi"); // BOM
        assert_eq!(normalize_text("a\r\nb"), "a\nb"); // CRLF -> LF
        assert_eq!(normalize_text("\tx"), "    x"); // tab -> 4 spaces
    }

    #[test]
    fn keeps_legitimate_non_ascii() {
        // Accented letters, CJK, and the ʻokina modifier letter are content,
        // not spoofs — leave them alone.
        assert_eq!(normalize_text("café"), "café");
        assert_eq!(normalize_text("日本語"), "日本語");
        assert_eq!(normalize_text("Hawaiʻi"), "Hawaiʻi");
    }

    #[test]
    fn folds_homoglyphs_in_mixed_script_words() {
        // Cyrillic а (U+0430) and о (U+043E) hidden in Latin words.
        assert_eq!(normalize_text("p\u{0430}ssword"), "password");
        assert_eq!(normalize_text("SELECT * FROM us\u{0435}rs"), "SELECT * FROM users");
        // Identifiers with underscores are one word.
        assert_eq!(normalize_text("user_p\u{0430}ss = 1"), "user_pass = 1");
        // Greek capital Omicron and Cyrillic capitals in a hex literal.
        assert_eq!(normalize_text("0x\u{0410}\u{0412}"), "0xAB");
        assert_eq!(normalize_text("G\u{039F}"), "GO");
        // Surrounding punctuation and other words are untouched.
        assert_eq!(normalize_text("(c\u{043E}unt, \u{0442}\u{044B})"), "(count, \u{0442}\u{044B})");
    }

    #[test]
    fn keeps_single_script_and_distinct_letters() {
        // Whole Cyrillic and Greek words are real text, even in code.
        assert_eq!(normalize_text("print(\"привет\")"), "print(\"привет\")");
        assert_eq!(normalize_text("рос"), "рос"); // all look-alikes, no Latin
        assert_eq!(normalize_text("ΟΚ"), "ΟΚ"); // Greek-only word
        // Letters with no Latin twin keep the word in its own script, even
        // with a stray Latin letter (a common Russian typing slip).
        assert_eq!(normalize_text("cтол"), "cтол");
        // Distinct Greek letters used as symbols stay.
        assert_eq!(normalize_text("delay_μs = 5"), "delay_μs = 5");
        assert_eq!(normalize_text("α = 0.5"), "α = 0.5");
        assert_eq!(normalize_text("Δt"), "Δt");
        // Accented Latin is still Latin, and is kept.
        assert_eq!(normalize_text("café"), "café");
    }

    #[test]
    fn single_char_folding_matches() {
        assert_eq!(fold_char_to_str('\u{00A0}'), " ");
        assert_eq!(fold_char_to_str('\u{2019}'), "'");
        assert_eq!(fold_char_to_str('\u{2014}'), "-");
        assert_eq!(fold_char_to_str('\t'), "    ");
        assert_eq!(fold_char_to_str('\u{200B}'), "");
        assert_eq!(fold_char_to_str('x'), "x");
    }
}
