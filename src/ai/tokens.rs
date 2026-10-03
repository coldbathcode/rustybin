//! Deterministic token approximation shared by every provider.
//!
//! The text is cut into "pieces" that concatenate back to the original text:
//! - a run of letters/digits is split into chunks of at most 4 characters
//!   (so a word costs `ceil(chars / 4)` tokens, minimum 1);
//! - every other non-whitespace character (punctuation, symbols, emoji) is one piece;
//! - whitespace is attached to the piece that follows it (like BPE tokenizers
//!   that encode " word"); trailing whitespace is a piece of its own.
//!
//! Usage figures, `max_tokens` truncation and streaming chunks all use the
//! same pieces, so `completion_tokens` always equals the number of streamed
//! text deltas.

/// Split `text` into token pieces (concatenating them yields `text`).
pub fn pieces(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize; // start of the current (unfinished) piece
    let mut word_chars = 0usize; // alphanumeric chars in the current piece
    let mut in_word = false;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if in_word {
                out.push(&text[start..i]);
                start = i;
                in_word = false;
                word_chars = 0;
            }
            continue;
        }
        if c.is_alphanumeric() {
            if in_word && word_chars == 4 {
                out.push(&text[start..i]);
                start = i;
                word_chars = 0;
            }
            in_word = true;
            word_chars += 1;
            continue;
        }
        // Symbol: closes a running word, then is a piece of its own
        // (carrying any whitespace before it).
        if in_word {
            out.push(&text[start..i]);
            start = i;
            in_word = false;
            word_chars = 0;
        }
        let end = i + c.len_utf8();
        out.push(&text[start..end]);
        start = end;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// Number of tokens in `text` (0 for the empty string).
pub fn count(text: &str) -> u32 {
    u32::try_from(pieces(text).len()).unwrap_or(u32::MAX)
}

/// Keep at most `max` tokens of `text`. Returns the kept text and whether
/// anything was cut.
pub fn truncate(text: &str, max: u32) -> (String, bool) {
    let p = pieces(text);
    let max = max as usize;
    if p.len() <= max {
        return (text.to_string(), false);
    }
    (p[..max].concat(), true)
}

/// Tokens charged for one image part (a fixed figure, like a low-detail image).
pub const IMAGE_TOKENS: u32 = 85;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces_round_trip() {
        for s in [
            "",
            "hello",
            "Hello, world!",
            "  leading and trailing  ",
            "internationalization is long",
            "emoji 🙂 and ünïcödé",
            "a\n\nb\tc",
        ] {
            assert_eq!(pieces(s).concat(), s);
        }
    }

    #[test]
    fn counts() {
        assert_eq!(count(""), 0);
        assert_eq!(count("hi"), 1);
        assert_eq!(count("hello world"), 4); // hell|o| worl|d
        assert_eq!(count("Hello, how are you?"), 7);
        // ceil(20/4) = 5
        assert_eq!(count("internationalization"), 5);
        assert_eq!(count("a b c"), 3);
    }

    #[test]
    fn truncation() {
        let (t, cut) = truncate("one two three four", 2);
        assert!(cut);
        assert_eq!(t, "one two");
        let (t, cut) = truncate("one", 5);
        assert!(!cut);
        assert_eq!(t, "one");
    }
}
