//! Prologue scanner used by the `sparql_prefix_prologue` fuzz target.
//!
//! Kept in line with `src/sparql/prologue.rs`. The harness cannot link the
//! extension crate, so the scan is duplicated here and checked against the
//! same crash input.

const REGISTERED: &[(&str, &str)] = &[
    ("ex", "https://example.test/"),
    ("schema", "https://schema.org/"),
];

/// Prepend registered prefixes that the formal prologue does not already
/// declare, then check that local declarations were neither dropped nor
/// duplicated. Parsing the result must not panic.
pub fn check_prefix_prologue(query: &str) {
    let resolved = inject_missing(query);
    assert!(
        resolved.ends_with(query),
        "injection rewrote the query body"
    );

    let locals = local_prefixes(query);
    let after = local_prefixes(&resolved);
    let header_len = resolved.len() - query.len();
    let header = &resolved[..header_len];
    // A textual `PREFIX {name}:` search is the wrong oracle: SPARQL allows
    // any whitespace (the crash input uses CR), mixed keyword case, and no
    // space before `<iri>`. Counting with the same scanner still fails when
    // injection adds or drops a declaration of a prefix the query already
    // declared.
    for name in &locals {
        let before_count = locals.iter().filter(|value| *value == name).count();
        let after_count = after.iter().filter(|value| *value == name).count();
        assert_eq!(
            before_count, after_count,
            "local prefix {name:?} was duplicated or dropped"
        );
    }
    for (prefix, _) in REGISTERED {
        let was_local = locals.iter().any(|value| value == prefix);
        let injected = header.contains(&format!("PREFIX {prefix}:"));
        assert_eq!(
            injected, !was_local,
            "registered prefix {prefix} injection did not match the prologue"
        );
    }

    let _ = spargebra::SparqlParser::new().parse_query(&resolved);
}

fn inject_missing(query: &str) -> String {
    let local = local_prefixes(query);
    let mut header = String::new();
    for (prefix, expansion) in REGISTERED {
        if !local.iter().any(|value| value == prefix) {
            header.push_str("PREFIX ");
            header.push_str(prefix);
            header.push_str(": <");
            header.push_str(expansion);
            header.push_str(">\n");
        }
    }
    header + query
}

fn local_prefixes(query: &str) -> Vec<String> {
    let mut prefixes = Vec::new();
    let bytes = query.as_bytes();
    let mut at = 0;
    loop {
        at = skip_ws_comments(bytes, at);
        match next_decl(bytes, at) {
            Some((Some(name), end)) => {
                if !name.is_empty() {
                    prefixes.push(name);
                }
                at = end;
            }
            Some((None, end)) => at = end,
            None => break,
        }
    }
    prefixes
}

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
    use super::{check_prefix_prologue, inject_missing, local_prefixes};

    #[test]
    fn crash_4444eb7a_does_not_trip_the_duplicate_check() {
        let query = std::str::from_utf8(include_bytes!(
            "../corpus/sparql_prefix_prologue/crash-4444eb7a882d860f9f417fddf5eb782ad925264c"
        ))
        .expect("crash input is utf-8");
        assert!(local_prefixes(query).is_empty());
        check_prefix_prologue(query);
    }

    #[test]
    fn carriage_return_local_prefix_is_not_injected_again() {
        let query = "\rPREFIX\rex:\r<http://example.org/>\nSELECT * WHERE { ?s ex:p ?o }";
        assert_eq!(local_prefixes(query), vec!["ex".to_owned()]);
        let resolved = inject_missing(query);
        let header_len = resolved.len() - query.len();
        assert!(!resolved[..header_len].contains("ex:"));
        check_prefix_prologue(query);
    }

    #[test]
    fn default_prefix_does_not_hide_the_following_name() {
        let query = "PREFIX : <http://example.org/>\nPREFIX ex: <http://example.org/ex/>\nSELECT * WHERE { ?s ex:p ?o }";
        assert_eq!(local_prefixes(query), vec!["ex".to_owned()]);
        let resolved = inject_missing(query);
        assert_eq!(resolved.matches("PREFIX ex:").count(), 1);
        check_prefix_prologue(query);
    }
}
