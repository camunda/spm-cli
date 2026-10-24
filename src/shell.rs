//! Rendering the commands spm suggests (fix hints) so they paste safely.

use std::borrow::Cow;

/// `word` as one shell argument: unchanged when it holds only characters no
/// shell treats specially, else single-quoted (POSIX `'\''` escapes a quote;
/// on Windows, PowerShell's `''`). Skill names may hold spaces and shell
/// metacharacters, so every name or path interpolated into a suggested command
/// goes through here.
pub fn arg(word: &str) -> Cow<'_, str> {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_./:@+=,%".contains(c);
    if !word.is_empty() && word.chars().all(safe) {
        return Cow::Borrowed(word);
    }
    let escaped = if cfg!(windows) { "''" } else { r"'\''" };
    Cow::Owned(format!("'{}'", word.replace('\'', escaped)))
}

#[cfg(test)]
mod tests {
    use super::arg;

    #[test]
    fn leaves_plain_words_alone() {
        for w in ["greet", "my-skill", ".gemini/skills/a.b", "v1.2@x"] {
            assert_eq!(arg(w), w);
        }
    }

    #[test]
    fn quotes_spaces_metacharacters_and_empty_words() {
        assert_eq!(arg("my skill"), "'my skill'");
        assert_eq!(arg("a;rm -rf ~"), "'a;rm -rf ~'");
        assert_eq!(arg("$(x)`y`&"), "'$(x)`y`&'");
        assert_eq!(arg(""), "''");
        let quote = if cfg!(windows) {
            "'it''s'"
        } else {
            r"'it'\''s'"
        };
        assert_eq!(arg("it's"), quote);
    }
}
