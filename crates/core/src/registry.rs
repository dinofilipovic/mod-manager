//! Read-only access to Wine's registry files (`system.reg`, `user.reg`).
//!
//! The format is text: a `[Key\\Path] timestamp` header followed by
//! `"name"="value"` lines. Only string values are read; dword and hex values
//! are skipped.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::error::{Error, Result};

/// Returns the string values of `key`, or `None` if the key does not exist.
/// Key and value names are matched without regard to case, like Windows does.
pub fn read_key(file: &Path, key: &str) -> Result<Option<BTreeMap<String, String>>> {
    let text = fs::read_to_string(file).map_err(|e| Error::io(file, e))?;
    Ok(read_key_from_str(&text, key))
}

pub fn read_key_from_str(text: &str, key: &str) -> Option<BTreeMap<String, String>> {
    let mut values: Option<BTreeMap<String, String>> = None;
    for line in text.lines() {
        let line = line.trim_start();
        if let Some(header) = line.strip_prefix('[') {
            if values.is_some() {
                break;
            }
            let name = header.rsplit_once(']').map_or(header, |(name, _)| name);
            if unescape(name).eq_ignore_ascii_case(key) {
                values = Some(BTreeMap::new());
            }
            continue;
        }
        let Some(values) = values.as_mut() else {
            continue;
        };
        if let Some((name, value)) = parse_string_value(line) {
            values.insert(name.to_ascii_lowercase(), value);
        }
    }
    values
}

/// Parses `"name"="value"` or `@="value"`. Returns `None` for other value types.
fn parse_string_value(line: &str) -> Option<(String, String)> {
    let (name, rest) = if let Some(rest) = line.strip_prefix('@') {
        ("@".to_string(), rest)
    } else {
        let (name, rest) = take_quoted(line.strip_prefix('"')?)?;
        (name, rest)
    };
    let rest = rest
        .trim_start()
        .strip_prefix('=')?
        .trim_start()
        .strip_prefix('"')?;
    let (value, _) = take_quoted(rest)?;
    Some((name, value))
}

/// Reads up to the closing quote, handling escapes. `s` starts after the opening quote.
fn take_quoted(s: &str) -> Option<(String, &str)> {
    let mut out = String::new();
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &s[i + 1..])),
            '\\' => match chars.next()?.1 {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                '0' => out.push('\0'),
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    None
}

fn unescape(s: &str) -> String {
    s.replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER_REG: &str = r#"WINE REGISTRY Version 2
;; All keys relative to \\User\\S-1-5-21-0-0-0-1000

[Software\\Wine\\DllOverrides] 1729000000
#time=1db0000000000
"*version"="native,builtin"
"winmm"="native,builtin"
"d3d11"=dword:00000001

[Software\\Wine\\Other] 1729000000
"version"="builtin"
"#;

    #[test]
    fn reads_string_values_of_one_key() {
        let values = read_key_from_str(USER_REG, r"software\wine\dlloverrides").unwrap();
        assert_eq!(
            values.get("winmm").map(String::as_str),
            Some("native,builtin")
        );
        assert_eq!(
            values.get("*version").map(String::as_str),
            Some("native,builtin")
        );
        assert!(!values.contains_key("d3d11"));
        assert!(!values.contains_key("version"));
    }

    #[test]
    fn missing_key_is_none() {
        assert!(read_key_from_str(USER_REG, r"Software\Nope").is_none());
    }

    #[test]
    fn unescapes_windows_paths() {
        let reg = "[Software\\\\GOG.com\\\\Games\\\\1] 1\n\"path\"=\"C:\\\\GOG Games\\\\Cyberpunk 2077\"\n";
        let values = read_key_from_str(reg, r"Software\GOG.com\Games\1").unwrap();
        assert_eq!(values["path"], r"C:\GOG Games\Cyberpunk 2077");
    }
}
