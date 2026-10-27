//! Drift guards: the shipped agent skill documents every command and flag, and
//! the command overviews in the docs are generated from the CLI itself.

use crate::common::*;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Shipped agent skill (`skills/spm/SKILL.md`)
// ---------------------------------------------------------------------------

/// Absolute path of the user-facing skill this repo ships.
fn shipped_skill_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/spm")
}

/// The compact help screen of `spm <args>` (`-h`: one line per argument or
/// option, where `--help` spreads each over a paragraph), with LF line endings.
fn short_help(sb: &Sandbox, args: &[&str]) -> String {
    let mut full = args.to_vec();
    full.push("-h");
    sb.ok(&full).replace("\r\n", "\n")
}

/// One row of a compact help screen's `Arguments:` or `Options:` list.
#[derive(Debug, PartialEq)]
struct HelpEntry {
    /// `-g` for `-g, --global`.
    short: Option<String>,
    /// `--global`; `None` for a positional argument.
    long: Option<String>,
    /// The value placeholder: `<TAG>` for `--tag <TAG>`, or the whole head of
    /// a positional (`<NAME>`, `[VENDORS]...`).
    value: Option<String>,
    /// The `[possible values: …]` clap lists, from the arg's value parser.
    possible: Vec<String>,
}

/// The rows of the `title` list (`"Arguments:"`/`"Options:"`) of a compact
/// help screen. Only the head column (up to the first double space) is read as
/// the flag, so a `--foo` mentioned in a description is not mistaken for one.
fn help_entries(help: &str, title: &str) -> Vec<HelpEntry> {
    help.lines()
        .skip_while(|l| *l != title)
        .skip(1)
        .take_while(|l| l.starts_with(' '))
        .map(|line| {
            let line = line.trim();
            let (head, desc) = line.split_once("  ").unwrap_or((line, ""));
            let possible = desc
                .split_once("[possible values: ")
                .and_then(|(_, rest)| rest.split_once(']'))
                .map(|(list, _)| list.split(", ").map(str::to_string).collect())
                .unwrap_or_default();
            let mut entry = HelpEntry {
                short: None,
                long: None,
                value: None,
                possible,
            };
            if !head.starts_with('-') {
                entry.value = Some(head.to_string());
                return entry;
            }
            let mut value = Vec::new();
            for tok in head.split_whitespace() {
                let tok = tok.trim_end_matches(',');
                if tok.starts_with("--") {
                    entry.long = Some(tok.to_string());
                } else if tok.starts_with('-') {
                    entry.short = Some(tok.to_string());
                } else {
                    value.push(tok);
                }
            }
            entry.value = (!value.is_empty()).then(|| value.join(" "));
            entry
        })
        .collect()
}

/// The options of a compact help screen minus clap's own `--help` and
/// `-V, --version` (a value-taking `--version <RANGE>`, as on `spm add`, is kept).
fn help_options(help: &str) -> Vec<HelpEntry> {
    help_entries(help, "Options:")
        .into_iter()
        .filter(|e| {
            let long = e.long.as_deref();
            !(long == Some("--help")
                || (long == Some("--version") && e.short.as_deref() == Some("-V")))
        })
        .collect()
}

/// Flags an `spm … -h` screen advertises, as `(long, short)` pairs (for
/// example `("--global", Some("-g"))`), minus clap's built-in ones.
fn help_flags(sb: &Sandbox, args: &[&str]) -> Vec<(String, Option<String>)> {
    help_options(&short_help(sb, args))
        .into_iter()
        .filter_map(|e| Some((e.long?, e.short)))
        .collect()
}

/// The body of one `###` (or `##`) section of the skill, from its heading line
/// up to the next `##`/`###` heading. `heading` is matched as a line prefix.
fn skill_section<'a>(text: &'a str, heading: &str) -> &'a str {
    let start = text
        // `split_inclusive` keeps each line's real terminator, so byte offsets
        // stay exact for both LF and CRLF checkouts (Windows `core.autocrlf`).
        .split_inclusive('\n')
        .scan(0usize, |off, l| {
            let here = *off;
            *off += l.len();
            Some((here, l))
        })
        .find(|(_, l)| l.starts_with(heading))
        .map(|(o, _)| o)
        .unwrap_or_else(|| panic!("SKILL.md has no section starting with `{heading}`"));
    let body = &text[start..];
    let rest = &body[body.find('\n').map_or(body.len(), |n| n + 1)..];
    let end = rest
        .find("\n## ")
        .into_iter()
        .chain(rest.find("\n### "))
        .min()
        .map_or(body.len(), |n| body.len() - rest.len() + n);
    body[..end].trim_end()
}

#[test]
fn shipped_skill_has_valid_front_matter_and_passes_scan() {
    let sb = Sandbox::new();
    let dir = shipped_skill_dir();
    let text = std::fs::read_to_string(dir.join("SKILL.md")).expect("skills/spm/SKILL.md exists");

    // Agent Skills front matter: a leading `---` fenced block that must parse as
    // YAML (not just satisfy string checks), with `name` matching the directory
    // and a non-empty description within the loader's length budget.
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some("---"),
        "SKILL.md must open with front matter"
    );
    let mut front = String::new();
    let mut closed = false;
    for l in lines.by_ref() {
        if l == "---" {
            closed = true;
            break;
        }
        front.push_str(l);
        front.push('\n');
    }
    assert!(closed, "SKILL.md front matter has no closing `---`");

    #[derive(serde::Deserialize)]
    struct FrontMatter {
        name: String,
        description: String,
    }
    let fm: FrontMatter = serde_saphyr::from_str(&front)
        .unwrap_or_else(|e| panic!("front matter is not valid YAML: {e}\n---\n{front}---"));
    assert_eq!(
        fm.name, "spm",
        "front matter `name` must match the skill dir"
    );
    assert!(
        !fm.description.trim().is_empty() && fm.description.len() <= 1024,
        "description must be non-empty and <= 1024 chars: {:?}",
        fm.description
    );

    // spm's own scanner must find nothing to block (or warn about).
    let out = sb.ok(&["scan", dir.to_str().unwrap()]);
    assert!(out.contains("no suspicious patterns"), "{out}");
}

/// Drift guard: the skill must document every subcommand and every long flag the
/// CLI exposes, so adding a command or flag without updating the skill fails CI.
#[test]
fn shipped_skill_documents_every_command_and_flag() {
    let sb = Sandbox::new();
    let text = std::fs::read_to_string(shipped_skill_dir().join("SKILL.md")).unwrap();
    assert_skill_documents_cli(&sb, &text);
}

/// The same check must hold for a CRLF checkout of the skill (Windows with
/// `core.autocrlf=true`), so section extraction may not depend on `\n` byte math.
#[test]
fn shipped_skill_checks_are_line_ending_independent() {
    let sb = Sandbox::new();
    let lf = std::fs::read_to_string(shipped_skill_dir().join("SKILL.md"))
        .unwrap()
        .replace("\r\n", "\n");
    let crlf = lf.replace('\n', "\r\n");
    for heading in ["### `spm add`", "### `spm remove`", "## Targets"] {
        assert_eq!(
            skill_section(&crlf, heading).replace("\r\n", "\n"),
            skill_section(&lf, heading),
            "section `{heading}` differs between LF and CRLF"
        );
    }
    assert_skill_documents_cli(&sb, &crlf);
}

/// A leaf command: its path (`["target", "add"]`) and its visible aliases.
struct Leaf {
    path: Vec<String>,
    aliases: Vec<String>,
}

/// Every leaf command, discovered by following each `Commands:` block in the
/// help recursively, so a new nested subcommand is picked up without touching
/// the tests. Aliases are read from the parent's `[alias: …]`/`[aliases: …]`.
fn leaf_commands(sb: &Sandbox, prefix: &[&str]) -> Vec<Leaf> {
    let help = short_help(sb, prefix);
    let children: Vec<(String, Vec<String>)> = help
        .lines()
        .skip_while(|l| *l != "Commands:")
        .skip(1)
        .take_while(|l| l.starts_with(' '))
        .filter_map(|l| {
            let name = l.split_whitespace().next()?.to_string();
            let aliases = ["[alias: ", "[aliases: "]
                .iter()
                .find_map(|tag| l.split_once(tag))
                .and_then(|(_, rest)| rest.split_once(']'))
                .map(|(list, _)| list.split(", ").map(str::to_string).collect())
                .unwrap_or_default();
            Some((name, aliases))
        })
        .filter(|(name, _)| name != "help")
        .collect();
    children
        .into_iter()
        .flat_map(|(name, aliases)| {
            let mut next = prefix.to_vec();
            next.push(&name);
            let leaves = leaf_commands(sb, &next);
            if leaves.is_empty() {
                vec![Leaf {
                    path: next.iter().map(|s| s.to_string()).collect(),
                    aliases,
                }]
            } else {
                leaves
            }
        })
        .collect()
}

/// [`leaf_commands`] of the whole CLI, guarded against a vacuous parse.
fn all_leaf_commands(sb: &Sandbox) -> Vec<Leaf> {
    let leaves = leaf_commands(sb, &[]);
    let paths: Vec<String> = leaves.iter().map(|l| l.path.join(" ")).collect();
    assert!(
        leaves.len() >= 11 && paths.iter().any(|p| p == "target add"),
        "parsed too few commands: {paths:?}"
    );
    leaves
}

fn assert_skill_documents_cli(sb: &Sandbox, text: &str) {
    let leaves = all_leaf_commands(sb);
    // Guard the parsers themselves: a silent empty parse would make the checks
    // below vacuous, and `add --version <RANGE>` is a real selector, unlike
    // clap's built-in `-V, --version`.
    let add_flags = help_flags(sb, &["add"]);
    assert!(
        ["--force", "--version"]
            .iter()
            .all(|f| add_flags.iter().any(|(l, _)| l == f)),
        "{add_flags:?}"
    );
    assert!(skill_section(text, "### `spm add`").contains("--force"));

    let invocations: Vec<(Vec<&str>, String)> = leaves
        .iter()
        .map(|leaf| {
            (
                leaf.path.iter().map(String::as_str).collect(),
                format!("### `spm {}`", leaf.path.join(" ")),
            )
        })
        .collect();
    for (args, heading) in &invocations {
        // Each command's flags must appear in that command's own section, not
        // merely somewhere else in the skill.
        let section = skill_section(text, heading);
        for (long, short) in help_flags(sb, args) {
            let documented = section.contains(&long)
                || short.is_some_and(|s| section.contains(&format!("[{s}]")));
            assert!(
                documented,
                "`{heading}` section does not mention `{long}` (from `spm {} --help`)",
                args.join(" ")
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Generated command overviews (README.md, docs/reference/cli-commands.md)
// ---------------------------------------------------------------------------

/// The docs holding a command overview generated by [`cli_overview`].
const OVERVIEW_DOCS: &[&str] = &["README.md", "docs/reference/cli-commands.md"];

/// The comments fencing a generated command overview (matched as line prefixes).
const OVERVIEW_START: &str = "<!-- cli-overview:start";
const OVERVIEW_END: &str = "<!-- cli-overview:end";

/// Generated lines are wrapped (with a trailing ` \`) to stay within this width.
const OVERVIEW_WIDTH: usize = 80;

/// The line range strictly between the [`OVERVIEW_START`] and [`OVERVIEW_END`]
/// comments of `doc` (named `file` in errors).
fn overview_span(doc: &str, file: &str) -> std::ops::Range<usize> {
    let lines: Vec<&str> = doc.lines().collect();
    let find = |marker: &str, from: usize| {
        lines[from..]
            .iter()
            .position(|l| l.trim_start().starts_with(marker))
            .map(|i| from + i)
            .unwrap_or_else(|| panic!("{file}: no `{marker} … -->` comment"))
    };
    let start = find(OVERVIEW_START, 0);
    let end = find(OVERVIEW_END, start + 1);
    start + 1..end
}

/// The overview currently between the markers of `doc`, with `\n` line endings.
fn overview_block(doc: &str, file: &str) -> String {
    doc.lines()
        .skip(overview_span(doc, file).start)
        .take(overview_span(doc, file).len())
        .flat_map(|l| [l, "\n"])
        .collect()
}

/// `<NAME>`, `[VENDORS]...`: clap's placeholder with the name inside the
/// brackets swapped for the possible values, if any.
fn with_values(entry: &HelpEntry, placeholder: &str) -> String {
    if entry.possible.is_empty() {
        return placeholder.to_string();
    }
    let close = placeholder.find(['>', ']']).expect("bracketed placeholder");
    format!(
        "{}{}{}",
        &placeholder[..1],
        entry.possible.join("|"),
        &placeholder[close..]
    )
}

/// How clap names an entry in its `Usage:` line and its errors: `NAME` for a
/// positional `[NAME]`/`<NAME>...`, otherwise the long (or short) flag.
fn entry_key(entry: &HelpEntry) -> String {
    match entry.long.as_ref().or(entry.short.as_ref()) {
        Some(flag) => flag.clone(),
        None => entry
            .value
            .as_deref()
            .expect("positional placeholder")
            .trim_end_matches("...")
            .trim_matches(['<', '>', '[', ']'])
            .to_string(),
    }
}

/// An entry without its optional brackets: `--tag <TAG>`, `-g`, `NAME`. The
/// form it takes as one alternative of a `[a|b]`/`<a|b>` choice.
fn overview_bare(entry: &HelpEntry) -> String {
    let Some(flag) = entry.short.as_ref().or(entry.long.as_ref()) else {
        return entry_key(entry);
    };
    match &entry.value {
        Some(value) => format!("{flag} {}", with_values(entry, value)),
        None => flag.clone(),
    }
}

/// How one positional argument or option of a leaf command reads in the
/// overview on its own: clap's own placeholder, with its possible values
/// spelled out.
fn overview_arg(entry: &HelpEntry) -> String {
    match (&entry.long, &entry.short) {
        (None, None) => with_values(
            entry,
            entry.value.as_deref().expect("positional placeholder"),
        ),
        _ => format!("[{}]", overview_bare(entry)),
    }
}

/// The required choices clap prints in a `Usage:` line (`<NAME|--all>` for an
/// `ArgGroup` that must be given exactly once), as the keys of their members.
fn usage_required_groups(help: &str) -> Vec<Vec<String>> {
    let usage = help
        .lines()
        .find_map(|l| l.strip_prefix("Usage: "))
        .unwrap_or_default();
    let mut groups = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    for (i, c) in usage.char_indices() {
        match c {
            '<' | '[' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            '>' | ']' => {
                depth = depth.saturating_sub(1);
                let token = &usage[start..=i];
                if depth == 0 && token.starts_with('<') {
                    let inner = &token[1..token.len() - 1];
                    let mut members = Vec::new();
                    let (mut d, mut from) = (0usize, 0usize);
                    for (j, ch) in inner.char_indices() {
                        match ch {
                            '<' | '[' => d += 1,
                            '>' | ']' => d = d.saturating_sub(1),
                            '|' if d == 0 => {
                                members.push(&inner[from..j]);
                                from = j + 1;
                            }
                            _ => {}
                        }
                    }
                    members.push(&inner[from..]);
                    if members.len() > 1 {
                        groups.push(
                            members
                                .iter()
                                .map(|m| {
                                    m.split_whitespace().next().unwrap_or_default().to_string()
                                })
                                .collect(),
                        );
                    }
                }
            }
            _ => {}
        }
    }
    groups
}

/// The pairs of options of `spm <path>` that clap refuses together ("the
/// argument … cannot be used with …"), probed on the binary itself so the
/// overview reads the real `conflicts_with`/`ArgGroup` rules. Conflicts are
/// checked before required arguments, so a probe that clap accepts may run the
/// command: `sb` should be a throwaway sandbox.
fn option_conflicts(sb: &Sandbox, path: &[&str], help: &str) -> Vec<(String, String)> {
    let options = help_options(help);
    let probe = |e: &HelpEntry| -> Vec<String> {
        let mut args = vec![entry_key(e)];
        if e.value.is_some() {
            args.push(e.possible.first().cloned().unwrap_or_else(|| "x".into()));
        }
        args
    };
    let mut conflicts = Vec::new();
    for (i, a) in options.iter().enumerate() {
        for b in &options[i + 1..] {
            let args: Vec<String> = path
                .iter()
                .map(|s| s.to_string())
                .chain(probe(a))
                .chain(probe(b))
                .collect();
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let out = sb.spm(&args);
            let err = stderr_of(&out);
            if !out.status.success()
                && err.starts_with("error: the argument '")
                && err.contains("' cannot be used with '")
            {
                conflicts.push((entry_key(a), entry_key(b)));
            }
        }
    }
    conflicts
}

/// The overview words for a leaf's entries: one per entry, except that
/// entries tied by a conflict or a required group collapse into one choice at
/// the position of the first of them — `<a|b>` when clap requires exactly one,
/// `[a|b]` otherwise, where an alternative of several mutually compatible
/// options lists each in its own brackets (`[[--name <N>] [--plugin]|--all]`).
fn overview_words(
    entries: &[HelpEntry],
    required: &[Vec<String>],
    conflicts: &[(String, String)],
) -> Vec<String> {
    let keys: Vec<String> = entries.iter().map(entry_key).collect();
    let index = |k: &str| {
        keys.iter()
            .position(|x| x == k)
            .unwrap_or_else(|| panic!("`{k}` is not in the help screen: {keys:?}"))
    };
    let n = entries.len();
    let mut conflict = vec![vec![false; n]; n];
    let mut tie = |a: usize, b: usize| {
        conflict[a][b] = true;
        conflict[b][a] = true;
    };
    for group in required {
        for (i, a) in group.iter().enumerate() {
            for b in &group[i + 1..] {
                tie(index(a), index(b));
            }
        }
    }
    for (a, b) in conflicts {
        tie(index(a), index(b));
    }
    // Connected components of the conflict graph, in first-seen order.
    let mut component = vec![usize::MAX; n];
    for start in 0..n {
        if component[start] != usize::MAX {
            continue;
        }
        let mut stack = vec![start];
        component[start] = start;
        while let Some(x) = stack.pop() {
            for y in 0..n {
                if conflict[x][y] && component[y] == usize::MAX {
                    component[y] = start;
                    stack.push(y);
                }
            }
        }
    }
    let mut words = Vec::new();
    for first in 0..n {
        if component[first] != first {
            continue;
        }
        let members: Vec<usize> = (0..n).filter(|&i| component[i] == first).collect();
        if members.len() == 1 {
            words.push(overview_arg(&entries[first]));
            continue;
        }
        // The members must split into alternatives: options within one are
        // compatible, options across two conflict. Anything else needs a
        // richer synopsis than `[a|b]` can say.
        let mut alternatives: Vec<Vec<usize>> = Vec::new();
        for &m in &members {
            match alternatives.iter_mut().find(|alt| !conflict[alt[0]][m]) {
                Some(alt) => alt.push(m),
                None => alternatives.push(vec![m]),
            }
        }
        for (i, a) in alternatives.iter().enumerate() {
            for (j, b) in alternatives.iter().enumerate() {
                for &x in a {
                    for &y in b {
                        assert!(
                            x == y || conflict[x][y] == (i != j),
                            "`{}` and `{}` cannot be rendered as a choice between \
                             alternatives {:?}: extend the overview renderer",
                            keys[x],
                            keys[y],
                            members.iter().map(|&m| &keys[m]).collect::<Vec<_>>()
                        );
                    }
                }
            }
        }
        let names: Vec<&String> = members.iter().map(|&m| &keys[m]).collect();
        let is_required = match required
            .iter()
            .find(|g| g.iter().any(|k| names.contains(&k)))
        {
            Some(group) => {
                assert!(
                    group.len() == names.len() && alternatives.iter().all(|a| a.len() == 1),
                    "required group {group:?} is tied to other conflicts {names:?}: \
                     extend the overview renderer"
                );
                true
            }
            None => false,
        };
        let body: Vec<String> = alternatives
            .iter()
            .map(|alt| match alt.as_slice() {
                [one] => overview_bare(&entries[*one]),
                many => many
                    .iter()
                    .map(|&m| overview_arg(&entries[m]))
                    .collect::<Vec<_>>()
                    .join(" "),
            })
            .collect();
        let (open, close) = if is_required { ('<', '>') } else { ('[', ']') };
        words.push(format!("{open}{}{close}", body.join("|")));
    }
    words
}

/// `# <summary>` and the wrapped `spm <command> <args> <options>` line(s) for
/// one leaf command, from its compact help screen (`help`) and the pairs of
/// its options clap refuses together (`conflicts`, by [`entry_key`]).
fn overview_entry(leaf: &Leaf, help: &str, conflicts: &[(String, String)]) -> String {
    let summary = help.lines().next().unwrap_or_default().trim();
    let mut out = format!("# {summary}");
    match leaf.aliases.as_slice() {
        [] => {}
        [one] => out.push_str(&format!(" (alias: {one})")),
        many => out.push_str(&format!(" (aliases: {})", many.join(", "))),
    }
    out.push('\n');
    let entries: Vec<HelpEntry> = help_entries(help, "Arguments:")
        .into_iter()
        .chain(help_options(help))
        .collect();
    let words: Vec<String> = std::iter::once("spm".to_string())
        .chain(leaf.path.iter().cloned())
        .chain(overview_words(
            &entries,
            &usage_required_groups(help),
            conflicts,
        ))
        .collect();
    let mut line = String::new();
    for word in words {
        if !line.trim().is_empty() && line.len() + 1 + word.len() + " \\".len() > OVERVIEW_WIDTH {
            out.push_str(&line);
            out.push_str(" \\\n");
            line = "   ".to_string();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    out.push_str(&line);
    out.push('\n');
    out
}

/// The command overview: a ```bash block with every leaf command, its
/// arguments and options and its one-line summary, all read from the CLI's
/// own help and argument rules, so it can't drift from the clap definitions.
fn cli_overview(sb: &Sandbox) -> String {
    // Conflict probes clap accepts may run the command: keep them out of `sb`.
    let probes = Sandbox::new();
    let entries: Vec<String> = all_leaf_commands(sb)
        .iter()
        .map(|leaf| {
            let path: Vec<&str> = leaf.path.iter().map(String::as_str).collect();
            let help = short_help(sb, &path);
            overview_entry(leaf, &help, &option_conflicts(&probes, &path, &help))
        })
        .collect();
    format!("```bash\n{}```\n", entries.join("\n"))
}

/// The command overviews in the README and the CLI reference are exactly the
/// one [`cli_overview`] generates. `SPM_BLESS=1` rewrites them in place.
#[test]
fn cli_overview_blocks_match_the_cli() {
    let sb = Sandbox::new();
    let expected = cli_overview(&sb);
    // Guard the generator: the target list comes from the CLI's value parser,
    // which must offer exactly the targets the `ai.json` schema accepts.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("schema/ai.schema.json")).unwrap())
            .unwrap();
    let targets: Vec<&str> = schema["properties"]["targets"]["items"]["enum"]
        .as_array()
        .expect("schema lists the targets")
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert!(
        expected.contains(&format!("[--target <{}>]", targets.join("|"))),
        "`spm init --target` must list the schema targets {targets:?}:\n{expected}"
    );
    for file in OVERVIEW_DOCS {
        let path = root.join(file);
        let doc = std::fs::read_to_string(&path).unwrap();
        if bless_requested() {
            std::fs::write(
                &path,
                splice_lines(&doc, overview_span(&doc, file), &expected),
            )
            .unwrap();
        }
        let doc = std::fs::read_to_string(&path).unwrap();
        assert!(
            overview_block(&doc, file) == expected,
            "the command overview in {file} differs from the CLI. Regenerate it with \
             `SPM_BLESS=1 cargo test cli_overview`, or put this between the \
             `{OVERVIEW_START}` and `{OVERVIEW_END}` comments:\n\n{expected}"
        );
    }
}

#[test]
fn overview_markers_splice_and_extract_the_block() {
    let doc = "intro\n```bash\nkept\n```\n<!-- cli-overview:start x -->\nstale\n\
               <!-- cli-overview:end -->\noutro\n";
    let body = "```bash\nspm a\n```\n";
    let blessed = splice_lines(doc, overview_span(doc, "doc"), body);
    assert_eq!(
        blessed,
        "intro\n```bash\nkept\n```\n<!-- cli-overview:start x -->\n```bash\nspm a\n```\n\
         <!-- cli-overview:end -->\noutro\n"
    );
    assert_eq!(overview_block(&blessed, "doc"), body);
    // A CRLF checkout (Windows `core.autocrlf`) reads the same block.
    assert_eq!(overview_block(&blessed.replace('\n', "\r\n"), "doc"), body);
}

#[test]
#[should_panic(expected = "no `<!-- cli-overview:end")]
fn overview_span_requires_the_end_marker() {
    overview_span("<!-- cli-overview:start -->\n```bash\n```\n", "doc");
}

#[test]
fn overview_entry_renders_args_options_and_possible_values() {
    let help = "\
Add things

Usage: spm.exe thing add [OPTIONS] <NAME> [KINDS]...

Arguments:
  <NAME>      The name
  [KINDS]...  Kinds [possible values: a, b]

Options:
      --tag <TAG>            Pin a tag; unlike --other
      --protocol <PROTOCOL>  Protocol [default: ssh] [possible values: ssh, https]
  -g, --global               Global
  -h, --help                 Print help (see more with '--help')
";
    let leaf = Leaf {
        path: vec!["thing".into(), "add".into()],
        aliases: vec!["a".into()],
    };
    assert_eq!(
        overview_entry(&leaf, help, &[]),
        "# Add things (alias: a)\n\
         spm thing add <NAME> [a|b]... [--tag <TAG>] [--protocol <ssh|https>] [-g]\n"
    );
    let long = Leaf {
        path: vec!["x".repeat(60)],
        aliases: vec![],
    };
    assert_eq!(
        overview_entry(&long, help, &[]),
        format!(
            "# Add things\nspm {} <NAME> \\\n    [a|b]... [--tag <TAG>] \
             [--protocol <ssh|https>] [-g]\n",
            "x".repeat(60)
        )
    );
}

#[test]
fn overview_entry_renders_required_alternatives_and_exclusive_options() {
    let help = "\
Drop things

Usage: spm.exe thing drop [OPTIONS] <NAME|--all>

Arguments:
  [NAME]  The name

Options:
      --all              Every thing
      --tag <TAG>        Tag
      --branch <BRANCH>  Branch
      --name <NAME>      Name
      --each             Each
      --plugin           Plugin
  -g, --global           Global
  -h, --help             Print help
";
    assert_eq!(
        usage_required_groups(help),
        vec![vec!["NAME".to_string(), "--all".to_string()]]
    );
    let leaf = Leaf {
        path: vec!["drop".into()],
        aliases: vec![],
    };
    let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
    let conflicts = [
        pair("--tag", "--branch"),
        pair("--name", "--each"),
        pair("--plugin", "--each"),
    ];
    // A required group reads as clap's `<NAME|--all>`, never `[NAME] [--all]`
    // (neither, or both); exclusive options read as one choice.
    assert_eq!(
        overview_entry(&leaf, help, &conflicts),
        "# Drop things\n\
         spm drop <NAME|--all> [--tag <TAG>|--branch <BRANCH>] \\\n    \
         [[--name <NAME>] [--plugin]|--each] [-g]\n"
    );
}

#[test]
#[should_panic(expected = "cannot be rendered as a choice")]
fn overview_entry_rejects_conflicts_it_cannot_render() {
    let help = "\
X

Usage: spm.exe x [OPTIONS]

Options:
      --a  A
      --b  B
      --c  C
      --d  D
";
    let leaf = Leaf {
        path: vec!["x".into()],
        aliases: vec![],
    };
    // A path of conflicts (A-B, B-C, C-D): C and D conflict, yet both combine
    // with A, so no `[a|b]` choice of compatible alternatives says that.
    let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
    overview_entry(
        &leaf,
        help,
        &[pair("--a", "--b"), pair("--b", "--c"), pair("--c", "--d")],
    );
}

/// Dogfood: the shipped skill installs through `spm add … --path skills/spm`
/// and lands as a loadable `SKILL.md` for the target vendor.
#[test]
fn shipped_skill_installs_with_spm_add_path() {
    let sb = Sandbox::new();
    let dest = sb.skill_repo.join("skills/spm");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::copy(shipped_skill_dir().join("SKILL.md"), dest.join("SKILL.md")).unwrap();
    sb.git(&["add", "-A"]);
    sb.git(&["commit", "-qm", "add spm skill"]);

    sb.ok(&["init", "--target", "copilot"]);
    let out = sb.spm(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "skills/spm",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("added skill spm"), "{stdout}");
    let installed = sb
        .project
        .join(".agents/skills/spm-managed-skills/spm/SKILL.md");
    assert!(installed.exists(), "skill not materialized");
    assert!(sb.read("ai.json").contains("\"path\": \"skills/spm\""));
}
