//! Lint policy guard: shipped code in `src/` never silences a lint. Warnings
//! are fixed, not suppressed (AGENTS.md "Always-green policy"), so any
//! `#[allow(..)]`, `#![allow(..)]`, `#[expect(..)]` or a `cfg_attr` wrapping
//! one fails here with its `file:line`. Test code (`#[cfg(test)]` items) and
//! comments/string literals are not shipped and are ignored.

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
        } else if (at(0) == Some('r') || (matches!(at(0), Some('b' | 'c')) && at(1) == Some('r')))
            && (i == 0 || !ident(s[i - 1]))
            && {
                let r = if at(0) == Some('r') { i } else { i + 1 };
                let hashes = s[r + 1..].iter().take_while(|&&c| c == '#').count();
                s.get(r + 1 + hashes) == Some(&'"')
            }
        {
            let r = if at(0) == Some('r') { i } else { i + 1 };
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
/// whitespace and raw-identifier prefixes (`r#allow` is `allow`) removed, and
/// the offset just past its closing `]`.
fn attribute(code: &[char], i: usize) -> Option<(String, usize)> {
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
                    let body: Vec<char> = code[start..j]
                        .iter()
                        .copied()
                        .filter(|c| !c.is_whitespace())
                        .collect();
                    return Some((unraw(&body), j + 1));
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// `body` with every raw-identifier prefix dropped: `r#` starting an
/// identifier (literals are already blanked, so no raw string remains).
fn unraw(body: &[char]) -> String {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::with_capacity(body.len());
    let mut k = 0;
    while k < body.len() {
        let raw =
            body[k] == 'r' && body.get(k + 1) == Some(&'#') && (k == 0 || !ident(body[k - 1]));
        if raw {
            k += 2;
        } else {
            out.push(body[k]);
            k += 1;
        }
    }
    out
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

/// Whether the attribute starting at `i` is an inner one (`#![..]`), which
/// applies to its enclosing module rather than the next item.
fn is_inner(code: &[char], i: usize) -> bool {
    code[i + 1..]
        .iter()
        .find(|c| !c.is_whitespace())
        .is_some_and(|&c| c == '!')
}

/// Whether a compacted attribute body silences a lint.
fn suppresses(body: &str) -> bool {
    let lint_level = |b: &str| b.starts_with("allow(") || b.starts_with("expect(");
    lint_level(body)
        || (body.starts_with("cfg_attr(")
            && (body.contains(",allow(") || body.contains(",expect(")))
}

/// 1-based lines of `src` holding a lint suppression outside test code.
fn suppressions(src: &str) -> Vec<usize> {
    let mut code = code_only(src);
    // Blank every `#[cfg(test)]` item: its whole attribute list (whatever the
    // order), signature and body. An item ends at a top-level `;`, `,` (a
    // field/arm/variant/parameter) or `{..}` block, or just before a `}`, `)`
    // or `]` closing the enclosing one.
    let mut i = 0;
    // Start of the run of attributes the one at `i` belongs to, and the end of
    // the previous attribute in it (only whitespace may separate two).
    let (mut run, mut last_end) = (0usize, None::<usize>);
    while i < code.len() {
        let Some((body, end)) = attribute(&code, i) else {
            i += 1;
            continue;
        };
        let inner = is_inner(&code, i);
        let joined = last_end.is_some_and(|e| code[e..i].iter().all(|c| c.is_whitespace()));
        if inner || !joined {
            run = i;
        }
        // An inner attribute applies to its module, not to the next item.
        last_end = (!inner).then_some(end);
        if !is_cfg_test(&body) {
            i = end;
            continue;
        }
        // Only an item (not a field, arm or parameter) has a generic list to
        // skip; elsewhere `<` and `>` are comparisons.
        let generics = code[end..]
            .iter()
            .collect::<String>()
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|w| !w.is_empty())
            .take(4)
            .any(|w| {
                matches!(
                    w,
                    "fn" | "impl" | "struct" | "enum" | "trait" | "type" | "union"
                )
            });
        let (mut j, mut depth, mut angle) = (end, 0usize, 0usize);
        while j < code.len() {
            let (prev, next) = (j.checked_sub(1).map(|p| code[p]), code.get(j + 1).copied());
            match code[j] {
                '(' | '[' => depth += 1,
                ')' | ']' if depth > 0 => depth -= 1,
                // Generic lists: not `<<`, `<=`, `->`, `=>` or `>=`.
                '<' if generics
                    && !matches!(prev, Some('<'))
                    && !matches!(next, Some('<' | '=')) =>
                {
                    angle += 1
                }
                '>' if generics && !matches!(prev, Some('-' | '=')) && next != Some('=') => {
                    angle = angle.saturating_sub(1)
                }
                ';' | ',' if depth == 0 && angle == 0 => break,
                ';' if depth == 0 => break,
                '}' | ')' | ']' if depth == 0 => {
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
        for c in &mut code[run..stop] {
            if *c != '\n' {
                *c = ' ';
            }
        }
        i = stop;
        last_end = None;
    }
    let mut lines = Vec::new();
    let mut i = 0;
    while i < code.len() {
        match attribute(&code, i) {
            Some((body, end)) => {
                if suppresses(&body) {
                    lines.push(code[..i].iter().filter(|&&c| c == '\n').count() + 1);
                }
                i = end;
            }
            None => i += 1,
        }
    }
    lines
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn src_never_suppresses_lints() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    assert!(
        files.iter().any(|f| f.ends_with("src/main.rs")),
        "scan found no sources: {files:?}"
    );
    let mut offenders = Vec::new();
    for file in &files {
        let src = std::fs::read_to_string(file).unwrap();
        for line in suppressions(&src) {
            let rel = file.strip_prefix(root).unwrap_or(file);
            offenders.push(format!("{}:{line}", rel.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "lint suppressions in shipped code (fix the warning instead):\n{}",
        offenders.join("\n")
    );
}

#[test]
fn suppressions_flags_every_attribute_form() {
    let src = "\
fn a() {}
#[allow(dead_code)]
fn b() {}
#![allow(clippy::all)]
# [ allow ( unused ) ]
#[expect(dead_code)]
#[cfg_attr(feature = \"x\", allow(unused))]
#[cfg(not(test))]
mod live { #[allow(unused)] fn c() {} }
#[cfg(any(test, unix))]
mod m { #[allow(x)] fn f() {} }
struct T { #[cfg(test)] a: u8, #[allow(dead_code)] b: u8 }
struct U { a: u8, #[cfg(test)] b: u8 }
#[allow(after_field)]
fn d(x: u8) { match x { #[cfg(test)] 0 => {} #[allow(unused)] _ => {} } }
#[derive(Debug)]
struct S;
";
    assert_eq!(suppressions(src), vec![2, 4, 5, 6, 7, 9, 11, 12, 14, 15]);
}

#[test]
fn suppressions_ignores_test_code_comments_and_literals() {
    let src = r####"
// #[allow(dead_code)]
/* #[allow(dead_code)] /* nested */ #[allow(x)] */
/// docs: #[allow(dead_code)]
const S: &str = "#[allow(dead_code)] \" #[allow(x)]";
const R: &str = r#"#[allow(dead_code)]"#;
const C: char = '"';
fn lifetime<'a>(x: &'a str) -> &'a str { x }
#[cfg(test)]
mod tests {
    #[allow(dead_code)]
    fn helper() { let _ = [0u8; 2]; }
}
#[cfg(test)]
#[allow(dead_code)]
fn test_only() {}
#[cfg(all(test, unix))]
mod unix_tests { #[allow(x)] fn h() {} }
#[cfg(test)] mod inline_tests { #[allow(x)] fn g() { let _ = (1, 2); } }
#[allow(after_tests)]
fn live() {}
"####;
    assert_eq!(suppressions(src), vec![20]);
}

/// Raw identifiers name the same attribute: `r#allow` silences a lint too.
#[test]
fn suppressions_flags_raw_identifier_attributes() {
    let src = "\
#[r#allow(dead_code)]
fn a() {}
#[cfg_attr(not(test), r#allow(dead_code))]
fn b() {}
#[r#expect(unused)]
fn c() {}
";
    assert_eq!(suppressions(src), vec![1, 3, 5]);
}

/// A test-only parameter ends at the `)` closing the parameter list; the
/// function body after it ships.
#[test]
fn suppressions_stops_a_test_only_item_at_its_enclosing_delimiter() {
    let src = "\
fn f(#[cfg(test)] _x: u8) {
    #[allow(unused_variables)]
    let x = 1;
}
fn g(a: [u8; 1], #[cfg(test)] _y: u8) -> u8 { #[allow(x)] let _ = 0; a[0] }
";
    assert_eq!(suppressions(src), vec![2, 5]);
}

/// The test-code exemption covers an item's whole attribute list, whatever
/// the order of its attributes.
#[test]
fn suppressions_ignores_attributes_before_a_test_only_cfg() {
    let src = "\
#[allow(dead_code)]
#[cfg(test)]
fn helper() {}
#[allow(dead_code)] /// docs
#[derive(Debug)] #[cfg(test)] struct T;
struct S { #[allow(dead_code)] #[cfg(test)] a: u8, b: u8 }
#[allow(after)]
fn live() {}
#[allow(before_live)]
fn live2() {}
#[cfg(test)]
fn t() {}
#![allow(inner)]
#[cfg(test)] mod m {}
";
    assert_eq!(suppressions(src), vec![7, 9, 13]);
}

/// A comma inside a generic parameter list does not end a test-only item, and
/// a raw C string does not derail the literal scanner.
#[test]
fn suppressions_handles_generics_and_raw_c_strings() {
    let src = "\
#[cfg(test)] fn helper<A, B>(_a: A, _b: B) -> Vec<(A, B)> { #[allow(x)] let _ = 0; vec![] }
#[cfg(test)] fn ret() -> Result<u8, u8> { #[allow(y)] Ok(0) }
const C: &core::ffi::CStr = cr#\"a\"b\"#;
#[allow(live)]
fn live() {}
fn m(x: u8) { match x { #[cfg(test)] 0 => x < 1, #[allow(arm)] _ => x > 1 }; }
";
    assert_eq!(suppressions(src), vec![4, 6]);
}
