//! Minimal parser for Valve's KeyValues text format (`libraryfolders.vdf`,
//! `appmanifest_*.acf`). Supports quoted and bare tokens, nested objects and
//! `//` comments, which is all Steam's own files use.

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// Looks up a child by key, ignoring case like Steam does.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(entries) => entries
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            Value::Str(_) => None,
        }
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            Value::Str(s) => Some(s),
            Value::Obj(_) => None,
        }
    }

    pub fn entries(&self) -> &[(String, Value)] {
        match self {
            Value::Obj(entries) => entries,
            Value::Str(_) => &[],
        }
    }
}

#[derive(Debug, PartialEq)]
enum Token {
    Str(String),
    Open,
    Close,
}

/// Parses a whole document into a root object.
pub fn parse(input: &str) -> Result<Value, String> {
    let tokens = tokenize(input)?;
    let mut pos = 0;
    let root = parse_entries(&tokens, &mut pos, false)?;
    Ok(Value::Obj(root))
}

fn parse_entries(
    tokens: &[Token],
    pos: &mut usize,
    nested: bool,
) -> Result<Vec<(String, Value)>, String> {
    let mut entries = Vec::new();
    loop {
        match tokens.get(*pos) {
            None if nested => return Err("unexpected end of input, missing '}'".into()),
            None => return Ok(entries),
            Some(Token::Close) if nested => {
                *pos += 1;
                return Ok(entries);
            }
            Some(Token::Close) => return Err("unexpected '}'".into()),
            Some(Token::Open) => return Err("expected key, found '{'".into()),
            Some(Token::Str(key)) => {
                *pos += 1;
                let value = match tokens.get(*pos) {
                    Some(Token::Str(s)) => {
                        *pos += 1;
                        Value::Str(s.clone())
                    }
                    Some(Token::Open) => {
                        *pos += 1;
                        Value::Obj(parse_entries(tokens, pos, true)?)
                    }
                    _ => return Err(format!("missing value for key '{key}'")),
                };
                entries.push((key.clone(), value));
            }
        }
    }
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '{' => {
                chars.next();
                tokens.push(Token::Open);
            }
            '}' => {
                chars.next();
                tokens.push(Token::Close);
            }
            '/' => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    while chars.next().is_some_and(|c| c != '\n') {}
                } else {
                    return Err("unexpected '/'".into());
                }
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                loop {
                    match chars.next() {
                        None => return Err("unterminated string".into()),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(other) => s.push(other),
                            None => return Err("unterminated escape".into()),
                        },
                        Some(other) => s.push(other),
                    }
                }
                tokens.push(Token::Str(s));
            }
            _ => {
                let mut s = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || c == '{' || c == '}' || c == '"' {
                        break;
                    }
                    s.push(c);
                    chars.next();
                }
                tokens.push(Token::Str(s));
            }
        }
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_library_folders() {
        let doc = parse(
            r#"
            // comment
            "libraryfolders"
            {
                "0"
                {
                    "path"		"C:\\Program Files (x86)\\Steam"
                    "apps" { "1091500" "0" }
                }
            }"#,
        )
        .unwrap();
        let lib = doc.get("LibraryFolders").unwrap().get("0").unwrap();
        assert_eq!(lib.get_str("path"), Some(r"C:\Program Files (x86)\Steam"));
        assert_eq!(lib.get("apps").unwrap().get_str("1091500"), Some("0"));
    }

    #[test]
    fn reports_unbalanced_braces() {
        assert!(parse(r#""a" { "b" "c""#).is_err());
        assert!(parse(r#""a" "b" }"#).is_err());
    }
}
