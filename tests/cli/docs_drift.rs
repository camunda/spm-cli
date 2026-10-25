//! Drift guards: the shipped agent skill and the command overviews document
//! every command and flag.

use crate::common::*;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Shipped agent skill (`skills/spm/SKILL.md`)
// ---------------------------------------------------------------------------

/// Absolute path of the user-facing skill this repo ships.
fn shipped_skill_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/spm")
}

/// Flags an `spm … --help` screen advertises, as `(long, short)` pairs (for
/// example `("--global", Some("-g"))`), minus clap's own `--help` and `-V, --version`
/// (a value-taking `--version <RANGE>`, as on `spm add`, is kept).
/// Only the flag column is parsed, so `--foo` mentioned in a description is not
/// mistaken for a flag.
fn help_flags(sb: &Sandbox, args: &[&str]) -> Vec<(String, Option<String>)> {
    let mut full = args.to_vec();
    full.push("--help");
    let help = sb.ok(&full);
    let mut flags = Vec::new();
    for line in help
        .lines()
        .skip_while(|l| !l.starts_with("Options:"))
        .skip(1)
        .take_while(|l| l.starts_with(' '))
    {
        let mut short = None;
        for tok in line.split_whitespace() {
            let tok = tok.trim_end_matches(',');
            if let Some(long) = tok.strip_prefix("--") {
                let builtin =
                    long == "help" || (long == "version" && short.as_deref() == Some("-V"));
                if !builtin {
                    flags.push((tok.to_string(), short.take()));
                }
                break;
            } else if tok.starts_with('-') {
                short = Some(tok.to_string());
            } else {
                break;
            }
        }
    }
    flags
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

/// Every leaf command path (`["init"]`, `["target", "add"]`, …), discovered by
/// following each `Commands:` block in `--help` recursively, so a new nested
/// subcommand is picked up without touching the test.
fn leaf_commands(sb: &Sandbox, prefix: &[&str]) -> Vec<Vec<String>> {
    let mut args = prefix.to_vec();
    args.push("--help");
    let help = sb.ok(&args);
    let children: Vec<String> = help
        .lines()
        .skip_while(|l| !l.starts_with("Commands:"))
        .skip(1)
        .take_while(|l| l.starts_with(' '))
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .filter(|c| c != "help")
        .collect();
    if children.is_empty() {
        return vec![prefix.iter().map(|s| s.to_string()).collect()];
    }
    children
        .iter()
        .flat_map(|c| {
            let mut next = prefix.to_vec();
            next.push(c.as_str());
            leaf_commands(sb, &next)
        })
        .collect()
}

fn assert_skill_documents_cli(sb: &Sandbox, text: &str) {
    let leaves = leaf_commands(sb, &[]);
    assert!(
        leaves.len() >= 11 && leaves.contains(&vec!["target".to_string(), "add".to_string()]),
        "parsed too few commands: {leaves:?}"
    );
    // Guard the parsers themselves: a silent empty parse would make the checks below vacuous.
    let add_flags = help_flags(sb, &["add"]);
    assert!(
        add_flags.iter().any(|(l, _)| l == "--force"),
        "{add_flags:?}"
    );
    assert!(skill_section(text, "### `spm add`").contains("--force"));

    let invocations: Vec<(Vec<&str>, String)> = leaves
        .iter()
        .map(|path| {
            (
                path.iter().map(String::as_str).collect(),
                format!("### `spm {}`", path.join(" ")),
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

/// Drift guard: the command overview block in the README and the CLI reference
/// must list every leaf command with every flag it accepts, so a new command or
/// flag can't silently go undocumented there. `--protocol`/`--protocol-fallback`
/// are shared by many commands and documented in prose instead.
#[test]
fn command_overviews_document_every_command_and_flag() {
    let sb = Sandbox::new();
    let leaves = leaf_commands(&sb, &[]);
    assert!(
        leaves.len() >= 11 && leaves.contains(&vec!["target".to_string(), "add".to_string()]),
        "parsed too few commands: {leaves:?}"
    );
    // `add --version <RANGE>` is a real selector, unlike clap's built-in `-V, --version`.
    assert!(
        help_flags(&sb, &["add"])
            .iter()
            .any(|(l, _)| l == "--version"),
        "help_flags dropped `add --version <RANGE>`"
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for (file, heading) in [
        ("README.md", "## Commands"),
        ("docs/reference/cli-commands.md", "# CLI Commands"),
    ] {
        let text = std::fs::read_to_string(root.join(file))
            .unwrap()
            .replace("\r\n", "\n");
        let after = &text[text
            .find(&format!("{heading}\n"))
            .unwrap_or_else(|| panic!("{file} has no `{heading}` heading"))..];
        let block = after
            .split("```bash\n")
            .nth(1)
            .and_then(|b| b.split("\n```").next())
            .unwrap_or_else(|| panic!("{file}: no ```bash block under `{heading}`"));
        // One entry per `spm …` line, plus its indented continuation lines.
        let mut entries: Vec<String> = Vec::new();
        for line in block.lines() {
            match entries.last_mut() {
                Some(e) if line.starts_with(' ') => e.push_str(line),
                _ => entries.push(line.to_string()),
            }
        }
        for path in &leaves {
            let prefix = format!("spm {} ", path.join(" "));
            let entry = entries
                .iter()
                .find(|e| e.starts_with(&prefix))
                .unwrap_or_else(|| panic!("{file}: command overview lacks `{}`", prefix.trim()));
            let args: Vec<&str> = path.iter().map(String::as_str).collect();
            for (long, short) in help_flags(&sb, &args) {
                if long == "--protocol" || long == "--protocol-fallback" {
                    continue;
                }
                let documented = entry.contains(&long)
                    || short.is_some_and(|s| entry.contains(&format!("[{s}]")));
                assert!(
                    documented,
                    "{file}: `{}` overview line does not mention `{long}`",
                    prefix.trim()
                );
            }
        }
    }
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
