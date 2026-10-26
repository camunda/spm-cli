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

    /// The words a POSIX `sh` gets when it evaluates `script` after a
    /// function `f` that prints each of its arguments NUL-terminated.
    #[cfg(unix)]
    fn sh_words(script: &str) -> Vec<String> {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("f() {{ printf '%s\\0' \"$@\"; }}\n{script}"))
            // A directory with files in it, so a stray glob would expand.
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
        assert!(out.status.success(), "sh failed on {script:?}: {out:?}");
        let mut words: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .split('\0')
            .map(str::to_owned)
            .collect();
        assert_eq!(words.pop().as_deref(), Some(""), "NUL-terminated output");
        words
    }

    /// A word a suggested command may interpolate: any text but NUL (no
    /// process argument can hold one), weighted towards short words mixing
    /// the characters left bare with ones a shell treats specially.
    #[cfg(unix)]
    fn word() -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::prelude::*;
        prop_oneof![
            "[-_./:a-z0-9~=+@,%*?#$]{0,4}",
            "[*?A-Za-z.]{1,3}",
            "[-'\"\\\\$`!*?\\[\\]{}();&|<>~# \t\n@,%=+a-z.:/]{0,12}",
            any::<String>().prop_map(|s| s.replace('\0', "")),
        ]
    }

    #[cfg(unix)]
    proptest::proptest! {
        #![proptest_config(crate::proptest_config(64))]

        /// Each `arg` reaches the shell as exactly the original word.
        #[test]
        fn arg_round_trips_through_sh(words in proptest::collection::vec(word(), 1..5)) {
            let line: Vec<_> = words.iter().map(|w| arg(w)).collect();
            proptest::prop_assert_eq!(sh_words(&format!("f {}", line.join(" "))), words);
        }

        /// `operands` yields the original words, after the `--` that ends a
        /// command's options exactly when one of them looks like an option.
        #[test]
        fn operands_round_trip_through_sh(words in proptest::collection::vec(word(), 1..5)) {
            let mut expected = words.clone();
            if words.iter().any(|w| w.starts_with('-')) {
                expected.insert(0, "--".into());
            }
            let line = operands(words.iter().map(String::as_str));
            proptest::prop_assert_eq!(sh_words(&format!("f {line}")), expected);
        }
    }
}
