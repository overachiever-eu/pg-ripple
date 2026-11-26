//! Safe, formal-prologue-only prefix resolution for SPARQL.
//!
//! The scanner follows the same prologue shape as spargebra: optional
//! whitespace and `#` comments (ending at CR or LF) between `BASE` / `PREFIX`
//! / `VERSION`, a `PNAME_NS`, and an `IRIREF`. A declaration counts only when
//! the IRI would be accepted as an IRIREF. The default prefix (`PREFIX :`)
//! is consumed so a later named prefix is still visible, and it is not
//! recorded — registered labels are never empty.

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};

/// Add registered declarations without inspecting the query body.
pub(crate) fn inject_registered_prefixes(query: &str) -> Cow<'_, str> {
    let mode = crate::SPARQL_PREFIX_MODE
        .get()
        .as_ref()
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_else(|| "strict".to_owned());
    if mode != "registered" {
        return Cow::Borrowed(query);
    }

    let local = local_prefixes(query);
    let declarations: BTreeMap<_, _> = crate::storage::list_prefixes()
        .into_iter()
        .filter(|(prefix, _)| !local.contains(prefix))
        .collect();
    if declarations.is_empty() {
        return Cow::Borrowed(query);
    }
    let mut prologue = String::new();
    for (prefix, expansion) in declarations {
        prologue.push_str("PREFIX ");
        prologue.push_str(&prefix);
        prologue.push_str(": <");
        prologue.push_str(&expansion);
        prologue.push_str(">\n");
    }
    Cow::Owned(format!("{prologue}{query}"))
}

pub(crate) fn has_local_prefix(query: &str, wanted: &str) -> bool {
    local_prefixes(query).contains(wanted)
}

/// Return the first form keyword after a formal BASE/PREFIX prologue.
pub(crate) fn first_form_keyword(query: &str) -> Option<String> {
    let bytes = query.as_bytes();
    let at = end_of_prologue(bytes);
    if at >= bytes.len() {
        return None;
    }
    let (keyword, _) = token(bytes, at)?;
    if keyword.is_empty() {
        return None;
    }
    Some(keyword.to_ascii_uppercase())
}

fn local_prefixes(query: &str) -> BTreeSet<String> {
    let mut prefixes = BTreeSet::new();
    let bytes = query.as_bytes();
    let mut at = 0;
    loop {
        at = skip_ws_comments(bytes, at);
        match next_decl(bytes, at) {
            Some((Some(name), end)) => {
                if !name.is_empty() {
                    prefixes.insert(name);
                }
                at = end;
            }
            Some((None, end)) => at = end,
            None => break,
        }
    }
    prefixes
}

fn end_of_prologue(bytes: &[u8]) -> usize {
    let mut at = 0;
    loop {
        at = skip_ws_comments(bytes, at);
        match next_decl(bytes, at) {
            Some((_, end)) => at = end,
            None => return at,
        }
    }
}

/// `Some((Some(name), end))` is a prefix declaration (`name` empty for the
/// default prefix). `Some((None, end))` is BASE or VERSION. `None` means the
/// prologue has ended.
fn next_decl(bytes: &[u8], at: usize) -> Option<(Option<String>, usize)> {
    if let Some(end) = parse_base_decl(bytes, at) {
        return Some((None, end));
    }
    if let Some((name, end)) = parse_prefix_decl(bytes, at) {
        return Some((Some(name), end));
    }
    if let Some(end) = parse_version_decl(bytes, at) {
        return Some((None, end));
    }
    None
}

fn parse_base_decl(bytes: &[u8], at: usize) -> Option<usize> {
    let i = keyword_end(bytes, at, b"base")?;
    parse_iriref(bytes, skip_ws_comments(bytes, i))
}

fn parse_prefix_decl(bytes: &[u8], at: usize) -> Option<(String, usize)> {
    let i = keyword_end(bytes, at, b"prefix")?;
    let (name, i) = parse_pname_ns(bytes, skip_ws_comments(bytes, i))?;
    let end = parse_iriref(bytes, skip_ws_comments(bytes, i))?;
    Some((name, end))
}

fn parse_version_decl(bytes: &[u8], at: usize) -> Option<usize> {
    let i = keyword_end(bytes, at, b"version")?;
    parse_sparql_string(bytes, skip_ws_comments(bytes, i))
}

fn keyword_end(bytes: &[u8], at: usize, kw: &[u8]) -> Option<usize> {
    let end = at.checked_add(kw.len())?;
    if end <= bytes.len() && bytes[at..end].eq_ignore_ascii_case(kw) {
        Some(end)
    } else {
        None
    }
}

fn parse_pname_ns(bytes: &[u8], at: usize) -> Option<(String, usize)> {
    if bytes.get(at) == Some(&b':') {
        return Some((String::new(), at + 1));
    }
    let (name, end) = parse_pn_prefix(bytes, at)?;
    if bytes.get(end) != Some(&b':') {
        return None;
    }
    Some((name, end + 1))
}

fn parse_pn_prefix(bytes: &[u8], at: usize) -> Option<(String, usize)> {
    let rest = std::str::from_utf8(bytes.get(at..)?).ok()?;
    let mut chars = rest.char_indices();
    let (_, first) = chars.next()?;
    if !is_pn_chars_base(first) {
        return None;
    }
    let mut last_good = first.len_utf8();
    for (idx, ch) in chars {
        if ch == '.' || is_pn_chars(ch) {
            if ch != '.' {
                last_good = idx + ch.len_utf8();
            }
        } else {
            break;
        }
    }
    Some((rest[..last_good].to_owned(), at + last_good))
}

fn parse_iriref(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes.get(at) != Some(&b'<') {
        return None;
    }
    let mut i = at + 1;
    while i < bytes.len() && bytes[i] != b'>' {
        if bytes[i] == b'\\' {
            let width = unicode_escape_width(bytes, i)?;
            i += width;
            continue;
        }
        let b = bytes[i];
        if b <= 0x20 || matches!(b, b'<' | b'"' | b'{' | b'}' | b'|' | b'^' | b'`') {
            return None;
        }
        i += 1;
    }
    if bytes.get(i) == Some(&b'>') {
        Some(i + 1)
    } else {
        None
    }
}

fn unicode_escape_width(bytes: &[u8], at: usize) -> Option<usize> {
    let mark = *bytes.get(at + 1)?;
    let hex_len = match mark {
        b'u' => 4,
        b'U' => 8,
        _ => return None,
    };
    let start = at + 2;
    let end = start + hex_len;
    if end <= bytes.len() && bytes[start..end].iter().all(u8::is_ascii_hexdigit) {
        Some(2 + hex_len)
    } else {
        None
    }
}

fn parse_sparql_string(bytes: &[u8], at: usize) -> Option<usize> {
    let quote = *bytes.get(at)?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    let mut i = at + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i = i.checked_add(2)?;
            if i > bytes.len() {
                return None;
            }
            continue;
        }
        if bytes[i] == quote {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

fn skip_ws_comments(bytes: &[u8], mut at: usize) -> usize {
    loop {
        while matches!(bytes.get(at), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            at += 1;
        }
        if bytes.get(at) != Some(&b'#') {
            return at;
        }
        at += 1;
        while at < bytes.len() && bytes[at] != b'\n' && bytes[at] != b'\r' {
            at += 1;
        }
    }
}

fn token(bytes: &[u8], mut at: usize) -> Option<(&str, usize)> {
    let start = at;
    while at < bytes.len() && !matches!(bytes[at], b' ' | b'\t' | b'\r' | b'\n') {
        at += 1;
    }
    std::str::from_utf8(bytes.get(start..at)?)
        .ok()
        .map(|s| (s, at))
}

fn is_pn_chars_base(c: char) -> bool {
    matches!(
        c,
        'A'..='Z'
            | 'a'..='z'
            | '\u{00C0}'..='\u{00D6}'
            | '\u{00D8}'..='\u{00F6}'
            | '\u{00F8}'..='\u{02FF}'
            | '\u{0370}'..='\u{037D}'
            | '\u{037F}'..='\u{1FFF}'
            | '\u{200C}'..='\u{200D}'
            | '\u{2070}'..='\u{218F}'
            | '\u{2C00}'..='\u{2FEF}'
            | '\u{3001}'..='\u{D7FF}'
            | '\u{F900}'..='\u{FDCF}'
            | '\u{FDF0}'..='\u{FFFD}'
    ) || ('\u{10000}'..='\u{EFFFF}').contains(&c)
}

fn is_pn_chars(c: char) -> bool {
    c == '_'
        || c == '-'
        || c.is_ascii_digit()
        || c == '\u{00B7}'
        || ('\u{0300}'..='\u{036F}').contains(&c)
        || ('\u{203F}'..='\u{2040}').contains(&c)
        || is_pn_chars_base(c)
}

#[cfg(test)]
mod tests {
    use super::{first_form_keyword, has_local_prefix};

    #[test]
    fn default_prefix_does_not_hide_a_later_named_prefix() {
        let query = "PREFIX : <http://example.org/>\nPREFIX ex: <http://example.org/ex/>\nSELECT * WHERE { ?s ex:p ?o }";
        assert!(has_local_prefix(query, "ex"));
        assert!(!has_local_prefix(query, ""));
        assert_eq!(first_form_keyword(query).as_deref(), Some("SELECT"));
    }

    #[test]
    fn carriage_return_glued_prefix_and_cr_comment_are_visible() {
        let cr = "\rPREFIX\rex:\r<http://example.org/>\nSELECT * WHERE { ?s ex:p ?o }";
        assert!(has_local_prefix(cr, "ex"));
        assert_eq!(first_form_keyword(cr).as_deref(), Some("SELECT"));

        let glued = "PREFIXex:<http://example.org/>SELECT * WHERE { ?s ?p ?o }";
        assert!(has_local_prefix(glued, "ex"));

        let comment = "PREFIX ex: <http://example.org/> # c\rPREFIX schema: <http://schema.org/>\nSELECT * WHERE { ?s schema:p ?o }";
        assert!(has_local_prefix(comment, "schema"));
        assert_eq!(first_form_keyword(comment).as_deref(), Some("SELECT"));
    }

    #[test]
    fn version_and_base_are_prologue_not_the_form_keyword() {
        let query = "VERSION \"1.2\"\nBASE<http://example.org/>PREFIX ex:<http://example.org/e/>SELECT * WHERE { ?s ex:p ?o }";
        assert!(has_local_prefix(query, "ex"));
        assert_eq!(first_form_keyword(query).as_deref(), Some("SELECT"));
    }

    #[test]
    fn malformed_iriref_is_not_a_local_prefix() {
        // fuzz/artifacts crash-4444eb7a882d860f9f417fddf5eb782ad925264c
        // (Scheduled Fuzz run 45387673027). The `>` sits inside an IRIREF
        // that also contains `<` and `"`, so this is not a declaration.
        let query = std::str::from_utf8(include_bytes!(
            "../../fuzz/corpus/sparql_prefix_prologue/crash-4444eb7a882d860f9f417fddf5eb782ad925264c"
        ))
        .expect("crash input is utf-8");
        assert!(!has_local_prefix(query, ""));
        assert!(!has_local_prefix(query, "ex"));
        assert!(!has_local_prefix(query, "schema"));
        assert_eq!(first_form_keyword(query).as_deref(), Some("PREFIX"));
    }

    #[test]
    fn unicode_escape_in_iriref_is_still_a_declaration() {
        let query = "PREFIX ex: <http://example.org/\\u0041>\nSELECT * WHERE { ?s ex:p ?o }";
        assert!(has_local_prefix(query, "ex"));
        assert_eq!(first_form_keyword(query).as_deref(), Some("SELECT"));
    }
}
