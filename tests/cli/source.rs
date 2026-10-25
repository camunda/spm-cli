//! Source scanning shared by the guards that read `src/` (lint policy,
//! layout, architecture): a comment/literal-blind, test-code-blind view of a
//! Rust file, so every guard judges exactly the code that ships.

use std::path::{Path, PathBuf};

/// `src` with every comment and string/char literal blanked to spaces
/// (newlines kept, so offsets still map to the same lines).
fn code_only(src: &str) -> Vec<char> {
    let s: Vec<char> = src.chars().collect();
    let mut out = s.clone();
    let blank = |out: &mut Vec<char>, from: usize, to: usize| {
        for c in &mut out[from..to.min(s.len())] {
            if *c != '\n' {
                *c = ' ';
            }
        }
    };
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut i = 0;
    while i < s.len() {
        let at = |k: usize| s.get(i + k).copied();
        if at(0) == Some('/') && at(1) == Some('/') {
            let end = s[i..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(s.len(), |p| i + p);
            blank(&mut out, i, end);
            i = end;
        } else if at(0) == Some('/') && at(1) == Some('*') {
            // Block comments nest in Rust.
            let (mut depth, mut j) = (0usize, i);
            while j < s.len() {
                if s[j] == '/' && s.get(j + 1) == Some(&'*') {
                    depth += 1;
                    j += 2;
                } else if s[j] == '*' && s.get(j + 1) == Some(&'/') {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            blank(&mut out, i, j);
            i = j;
        } else if (at(0) == Some('r') || (at(0) == Some('b') && at(1) == Some('r')))
            && (i == 0 || !ident(s[i - 1]))
            && {
                let r = if at(0) == Some('b') { i + 1 } else { i };
                let hashes = s[r + 1..].iter().take_while(|&&c| c == '#').count();
                s.get(r + 1 + hashes) == Some(&'"')
            }
        {
            let r = if at(0) == Some('b') { i + 1 } else { i };
            let hashes = s[r + 1..].iter().take_while(|&&c| c == '#').count();
            let mut j = r + 2 + hashes;
            while j < s.len() {
                if s[j] == '"'
                    && s[j + 1..]
                        .iter()
                        .take(hashes)
                        .filter(|&&c| c == '#')
                        .count()
                        == hashes
                {
                    j += 1 + hashes;
                    break;
                }
                j += 1;
            }
            blank(&mut out, i, j);
            i = j;
        } else if at(0) == Some('"') {
            let mut j = i + 1;
            while j < s.len() && s[j] != '"' {
                j += if s[j] == '\\' { 2 } else { 1 };
            }
            blank(&mut out, i, j + 1);
            i = j + 1;
        } else if at(0) == Some('\'') && (at(1) == Some('\\') || at(2) == Some('\'')) {
            // A char literal (a lifetime `'a` has no closing quote).
            let mut j = i + 1;
            while j < s.len() && s[j] != '\'' {
                j += if s[j] == '\\' { 2 } else { 1 };
            }
            blank(&mut out, i, j + 1);
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// An attribute starting at `i` (`#[..]` or `#![..]`): its body with all
/// whitespace removed and the offset just past its closing `]`.
pub fn attribute(code: &[char], i: usize) -> Option<(String, usize)> {
    let skip_ws = |mut j: usize| {
        while code.get(j).is_some_and(|c| c.is_whitespace()) {
            j += 1;
        }
        j
    };
    if code.get(i) != Some(&'#') {
        return None;
    }
    let mut j = skip_ws(i + 1);
    if code.get(j) == Some(&'!') {
        j = skip_ws(j + 1);
    }
    if code.get(j) != Some(&'[') {
        return None;
    }
    let (start, mut depth) = (j + 1, 0usize);
    while j < code.len() {
        match code[j] {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    let body = code[start..j].iter().filter(|c| !c.is_whitespace());
                    return Some((body.collect(), j + 1));
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// Whether a compacted attribute body is a `cfg` that enables its item only
/// under test: exactly `cfg(test)`, or `cfg(all(..))` with `test` as a direct
/// argument. `any(..)`/`not(..)` (and anything else) can ship, so they are not.
fn is_cfg_test(body: &str) -> bool {
    let Some(pred) = body.strip_prefix("cfg(").and_then(|p| p.strip_suffix(')')) else {
        return false;
    };
    if pred == "test" {
        return true;
    }
    let Some(args) = pred.strip_prefix("all(").and_then(|p| p.strip_suffix(')')) else {
        return false;
    };
    let (mut depth, mut start) = (0usize, 0usize);
    for (k, c) in args.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                if &args[start..k] == "test" {
                    return true;
                }
                start = k + 1;
            }
            _ => {}
        }
    }
    &args[start..] == "test"
}

/// `src` as it ships: [`code_only`], with every `#[cfg(test)]` item (its
/// attributes, signature and body) blanked too. Offsets still map to the same
/// lines.
pub fn shipped_code(src: &str) -> Vec<char> {
    let mut code = code_only(src);
    // Blank every `#[cfg(test)]` item: its attributes, signature and body. An
    // item ends at a top-level `;`, `,` (a field/arm/variant) or `{..}` block,
    // or just before a `}` closing the enclosing block.
    let mut i = 0;
    while i < code.len() {
        let Some((body, end)) = attribute(&code, i) else {
            i += 1;
            continue;
        };
        if !is_cfg_test(&body) {
            i = end;
            continue;
        }
        let (mut j, mut depth) = (end, 0usize);
        while j < code.len() {
            match code[j] {
                '(' | '[' => depth += 1,
                ')' | ']' => depth = depth.saturating_sub(1),
                ';' | ',' if depth == 0 => break,
                '}' if depth == 0 => {
                    j -= 1;
                    break;
                }
                '{' if depth == 0 => {
                    let mut braces = 0usize;
                    while j < code.len() {
                        match code[j] {
                            '{' => braces += 1,
                            '}' => {
                                braces -= 1;
                                if braces == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        j += 1;
                    }
                    break;
                }
                _ => {}
            }
            j += 1;
        }
        let stop = (j + 1).min(code.len());
        for c in &mut code[i..stop] {
            if *c != '\n' {
                *c = ' ';
            }
        }
        i = stop;
    }
    code
}

/// Every `.rs` file under `dir`, recursively.
pub fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}
