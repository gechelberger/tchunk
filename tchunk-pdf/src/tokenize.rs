use anyhow::Result;
use tiktoken_rs::CoreBPE;

pub trait Tokenizer {
    fn count(&self, text: &str) -> usize;
    fn name(&self) -> &str;
}

pub struct TiktokenTokenizer {
    bpe: CoreBPE,
    name: &'static str,
}

impl TiktokenTokenizer {
    pub fn new(name: &str) -> Result<Self> {
        match name {
            "cl100k_base" => Ok(Self {
                bpe: tiktoken_rs::cl100k_base()?,
                name: "cl100k_base",
            }),
            "o200k_base" => Ok(Self {
                bpe: tiktoken_rs::o200k_base()?,
                name: "o200k_base",
            }),
            other => anyhow::bail!("unknown tokenizer: {other} (supported: cl100k_base, o200k_base)"),
        }
    }
}

impl Tokenizer for TiktokenTokenizer {
    fn count(&self, text: &str) -> usize {
        self.bpe.encode_ordinary(text).len()
    }

    fn name(&self) -> &str {
        self.name
    }
}

pub struct WordCountTokenizer;

impl Tokenizer for WordCountTokenizer {
    fn count(&self, text: &str) -> usize {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .count()
    }

    fn name(&self) -> &str {
        "word_count"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_count_empty() {
        assert_eq!(WordCountTokenizer.count(""), 0);
    }

    #[test]
    fn word_count_whitespace_only() {
        assert_eq!(WordCountTokenizer.count("   \t\n  "), 0);
    }

    #[test]
    fn word_count_punctuation_only() {
        assert_eq!(WordCountTokenizer.count("!!! --- ,,,"), 0);
    }

    #[test]
    fn word_count_plain_words() {
        assert_eq!(WordCountTokenizer.count("the quick brown fox"), 4);
    }

    #[test]
    fn word_count_punctuation_between_words() {
        assert_eq!(WordCountTokenizer.count("hello,world"), 2);
    }

    #[test]
    fn word_count_leading_and_trailing_punctuation() {
        assert_eq!(WordCountTokenizer.count("...hello, world!"), 2);
    }

    #[test]
    fn word_count_collapses_runs_of_whitespace_and_punctuation() {
        assert_eq!(WordCountTokenizer.count("one  ,  two -- three"), 3);
    }

    #[test]
    fn word_count_numbers_are_words() {
        assert_eq!(WordCountTokenizer.count("page 42 of 100"), 4);
    }

    #[test]
    fn word_count_apostrophe_splits_contractions() {
        // "don't" has an apostrophe, which is non-alphanumeric, so it splits
        // into "don" + "t". Pinning this down: the tokenizer is intentionally
        // naive and doesn't special-case apostrophes.
        assert_eq!(WordCountTokenizer.count("don't"), 2);
    }

    #[test]
    fn word_count_name_is_word_count() {
        assert_eq!(WordCountTokenizer.name(), "word_count");
    }
}
