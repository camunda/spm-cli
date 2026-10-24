//! Rendering the commands spm suggests (fix hints) so they paste safely.

use std::borrow::Cow;

/// `word` as one shell argument: unchanged when it holds only characters no
/// POSIX shell, PowerShell or cmd treats specially (so not `@`, `,` or `%`),
/// else single-quoted (POSIX `'\''` escapes a quote; on Windows, PowerShell
/// doubles each of its quote characters). Skill names may hold spaces and
/// shell metacharacters, so every name or path interpolated into a suggested
/// command goes through here.
pub fn arg(word: &str) -> Cow<'_, str> {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_./:".contains(c);
    if !word.is_empty() && word.chars().all(safe) {
        return Cow::Borrowed(word);
    }
    let quoted: String = if cfg!(windows) {
        word.chars()
            .flat_map(|c| match c {
                '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => vec![c, c],
                _ => vec![c],
            })
            .collect()
    } else {
        word.replace('\'', r"'\''")
    };
    Cow::Owned(format!("'{quoted}'"))
}

/// `names` as the trailing operands of a suggested command, each quoted by
/// [`arg`] and after a `--` when one could be taken for an option (a skill
/// may be named `--all`). Callers put every option before this.
pub fn operands<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let names: Vec<&str> = names.into_iter().collect();
    let quoted: Vec<Cow<str>> = names.iter().map(|n| arg(n)).collect();
    if names.iter().any(|n| n.starts_with('-')) {
        format!("-- {}", quoted.join(" "))
    } else {
        quoted.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::{arg, operands};

    #[test]
    fn leaves_plain_words_alone() {
        for w in ["greet", "my-skill", ".gemini/skills/a.b", "-x", "C:/x"] {
            assert_eq!(arg(w), w);
        }
    }

    #[test]
    fn quotes_spaces_metacharacters_and_empty_words() {
        assert_eq!(arg("my skill"), "'my skill'");
        assert_eq!(arg("a;rm -rf ~"), "'a;rm -rf ~'");
        assert_eq!(arg("$(x)`y`&"), "'$(x)`y`&'");
        // PowerShell splats a leading `@`, makes arrays of `,`; cmd expands `%`.
        for w in ["@greet", "a,b", "%PATH%", "g[1]"] {
            assert_eq!(arg(w), format!("'{w}'"));
        }
        assert_eq!(arg(""), "''");
        let quote = if cfg!(windows) {
            "'it''s'"
        } else {
            r"'it'\''s'"
        };
        assert_eq!(arg("it's"), quote);
    }

    #[test]
    fn separates_option_like_operands() {
        assert_eq!(operands(["greet"]), "greet");
        assert_eq!(operands(["--all"]), "-- --all");
        assert_eq!(operands(["a b", "-x"]), "-- 'a b' -x");
    }
}
