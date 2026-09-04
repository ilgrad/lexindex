//! Character-level subsequence matching over the FST.
//!
//! `fst`'s own `Subsequence` automaton advances one *byte* at a time, which is wrong for non-ASCII
//! queries: `é` is `[C3 A9]`, and those two bytes appear — separately, in different characters — in
//! `àΩ` (`[C3 A0 CE A9]`), so the byte automaton reports a match. This one tracks the same single
//! integer (bytes of the query matched so far) but resets a *partial* character on mismatch, so a
//! query character only matches a whole haystack character.

use fst::Automaton;

pub(crate) struct UnicodeSubsequence<'a> {
    query: &'a [u8],
}

impl<'a> UnicodeSubsequence<'a> {
    pub(crate) fn new(query: &'a str) -> Self {
        Self {
            query: query.as_bytes(),
        }
    }

    /// Offset of the character that query byte `pos` belongs to — where a mismatch mid-character
    /// rewinds to. UTF-8 continuation bytes are exactly the `10xxxxxx` ones, so walking back over
    /// them lands on the lead byte; at a boundary this is `pos` itself.
    #[inline]
    fn char_start(&self, pos: usize) -> usize {
        let mut start = pos;
        while start > 0 && self.query[start] & 0xC0 == 0x80 {
            start -= 1;
        }
        start
    }
}

impl Automaton for UnicodeSubsequence<'_> {
    /// Bytes of the query matched so far; `query.len()` is the (absorbing) accepting state.
    type State = usize;

    #[inline]
    fn start(&self) -> usize {
        0
    }

    #[inline]
    fn is_match(&self, &state: &usize) -> bool {
        state == self.query.len()
    }

    #[inline]
    fn can_match(&self, _: &usize) -> bool {
        true // any state can still reach the end: the rest of the query may follow
    }

    #[inline]
    fn will_always_match(&self, &state: &usize) -> bool {
        state == self.query.len()
    }

    /// Rewinding to `char_start(state)` — rather than staying put, as a byte automaton does — is what
    /// makes the match character-aligned, and it never drops a match: a mismatch at a non-boundary
    /// position is a mismatch against a *continuation* byte, so the byte that failed is itself a
    /// continuation byte of the haystack's character and can never equal the query's next character,
    /// which necessarily starts with a lead byte. UTF-8's disjoint lead/continuation byte classes are
    /// the whole argument; the same self-synchronisation lets a mismatched lead byte simply stay.
    #[inline]
    fn accept(&self, &state: &usize, byte: u8) -> usize {
        if state == self.query.len() {
            return state;
        }
        if byte == self.query[state] {
            state + 1
        } else {
            self.char_start(state)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(query: &str, haystack: &str) -> bool {
        let a = UnicodeSubsequence::new(query);
        let mut state = a.start();
        for &b in haystack.as_bytes() {
            state = a.accept(&state, b);
        }
        a.is_match(&state)
    }

    /// The bug this automaton exists for: `é` is `[C3 A9]`, and `àΩ` is `[C3 A0 CE A9]`, so a
    /// byte-level subsequence automaton matches it. A character-level one must not.
    #[test]
    fn a_split_query_character_is_not_a_match() {
        assert!(!matches("é", "àΩ"));
        assert!(matches("é", "café"));
        assert!(matches("çé", "çafé"));
        assert!(!matches("éç", "çafé")); // order still matters
    }

    #[test]
    fn ascii_behaves_like_the_byte_automaton() {
        assert!(matches("ace", "abcde"));
        assert!(matches("", "anything"));
        assert!(!matches("aec", "abcde"));
        assert!(matches("abc", "abc"));
        assert!(!matches("abcd", "abc"));
    }

    /// A mismatch mid-character rewinds far enough to start the character over on the very next one.
    #[test]
    fn a_partial_character_restarts_cleanly() {
        assert!(matches("é", "èé")); // [C3 A8] then [C3 A9]: the first C3 must not strand the match
        assert!(matches("中", "口中"));
        assert!(matches("🎉", "🎈🎉"));
    }
}
