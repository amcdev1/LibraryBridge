//! A small, bounded parser for Valve KeyValues text files.
//!
//! Used for `libraryfolders.vdf` and `appmanifest_*.acf`. Both are read-only
//! inputs from disk that we treat as untrusted, so the parser has hard limits
//! on size, depth and token count and never recurses without a bound.

use std::fmt;
use std::fs;
use std::path::Path;

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_TOKENS: usize = 400_000;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// Look up a direct child by key, case-insensitively as Steam does.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(pairs) => pairs
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            Value::Str(_) => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Obj(_) => None,
        }
    }

    /// Direct children, in file order. Empty for a plain string.
    pub fn entries(&self) -> &[(String, Value)] {
        match self {
            Value::Obj(pairs) => pairs,
            Value::Str(_) => &[],
        }
    }
}

#[derive(Debug)]
pub struct ParseError {
    pub message: String,
    pub offset: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}

impl std::error::Error for ParseError {}

/// Read and parse a KeyValues file. The returned value is the implicit root
/// object, so a document with a single `"libraryfolders" { ... }` block gives
/// an object with one entry.
pub fn parse_file(path: &Path) -> Result<Value, String> {
    let meta = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{}: file is {} bytes, larger than the {MAX_FILE_BYTES} byte limit",
            path.display(),
            meta.len()
        ));
    }
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn parse(input: &str) -> Result<Value, ParseError> {
    let mut p = Parser {
        bytes: input.as_bytes(),
        pos: 0,
        tokens: 0,
    };
    let pairs = p.parse_pairs(0, true)?;
    p.skip_trivia();
    if p.pos < p.bytes.len() {
        return Err(p.err("unexpected trailing content"));
    }
    Ok(Value::Obj(pairs))
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    tokens: usize,
}

impl<'a> Parser<'a> {
    fn err(&self, message: &str) -> ParseError {
        ParseError {
            message: message.to_string(),
            offset: self.pos,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    /// Skip whitespace, `//` line comments and `#base`-style directives.
    fn skip_trivia(&mut self) {
        loop {
            while matches!(self.peek(), Some(b) if b.is_ascii_whitespace()) {
                self.pos += 1;
            }
            if self.bytes[self.pos..].starts_with(b"//") {
                while !matches!(self.peek(), None | Some(b'\n')) {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
    }

    fn parse_pairs(
        &mut self,
        depth: usize,
        at_root: bool,
    ) -> Result<Vec<(String, Value)>, ParseError> {
        if depth > MAX_DEPTH {
            return Err(self.err("nesting is deeper than the parser allows"));
        }
        let mut out = Vec::new();
        loop {
            self.skip_trivia();
            match self.peek() {
                None => {
                    if at_root {
                        return Ok(out);
                    }
                    return Err(self.err("file ended inside a block"));
                }
                Some(b'}') => {
                    if at_root {
                        return Err(self.err("closing brace without a matching block"));
                    }
                    self.pos += 1;
                    return Ok(out);
                }
                Some(_) => {}
            }

            let key = self.parse_token()?;
            self.skip_trivia();
            match self.peek() {
                Some(b'{') => {
                    self.pos += 1;
                    let inner = self.parse_pairs(depth + 1, false)?;
                    out.push((key, Value::Obj(inner)));
                }
                Some(_) => {
                    let value = self.parse_token()?;
                    out.push((key, Value::Str(value)));
                }
                None => return Err(self.err("key has no value")),
            }
        }
    }

    fn parse_token(&mut self) -> Result<String, ParseError> {
        self.tokens += 1;
        if self.tokens > MAX_TOKENS {
            return Err(self.err("file contains more tokens than the parser allows"));
        }
        match self.peek() {
            Some(b'"') => self.parse_quoted(),
            Some(_) => self.parse_bare(),
            None => Err(self.err("expected a token")),
        }
    }

    fn parse_quoted(&mut self) -> Result<String, ParseError> {
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            let byte = match self.peek() {
                None => return Err(self.err("unterminated quoted string")),
                Some(b) => b,
            };
            self.pos += 1;
            match byte {
                b'"' => return Ok(out),
                b'\\' => {
                    let escaped = match self.peek() {
                        None => return Err(self.err("unterminated escape sequence")),
                        Some(b) => b,
                    };
                    self.pos += 1;
                    match escaped {
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'\\' => out.push('\\'),
                        b'"' => out.push('"'),
                        other => {
                            // Valve's own writer emits unknown escapes literally.
                            out.push('\\');
                            out.push(other as char);
                        }
                    }
                }
                _ => self.push_utf8(&mut out, byte),
            }
        }
    }

    fn parse_bare(&mut self) -> Result<String, ParseError> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b.is_ascii_whitespace() || b == b'{' || b == b'}' || b == b'"' {
                break;
            }
            self.pos += 1;
        }
        if start == self.pos {
            return Err(self.err("expected a token"));
        }
        Ok(String::from_utf8_lossy(&self.bytes[start..self.pos]).into_owned())
    }

    /// Rebuild a UTF-8 char that `parse_quoted` walked into byte by byte.
    fn push_utf8(&mut self, out: &mut String, first: u8) {
        if first < 0x80 {
            out.push(first as char);
            return;
        }
        let extra = if first >= 0xf0 {
            3
        } else if first >= 0xe0 {
            2
        } else {
            1
        };
        let start = self.pos - 1;
        let end = core::cmp::min(start + 1 + extra, self.bytes.len());
        out.push_str(&String::from_utf8_lossy(&self.bytes[start..end]));
        self.pos = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_modern_libraryfolders_file() {
        let text = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"/home/alice/.local/share/Steam"
		"label"		""
		"apps"
		{
			"228980"		"320737"
		}
	}
	"1"
	{
		"path"		"/run/media/alice/Games/SteamLibrary"
		"label"		"Games"
	}
}
"#;
        let root = parse(text).unwrap();
        let folders = root.get("libraryfolders").unwrap();
        assert_eq!(folders.entries().len(), 2);
        assert_eq!(
            folders.get("1").unwrap().get("path").unwrap().as_str(),
            Some("/run/media/alice/Games/SteamLibrary")
        );
    }

    #[test]
    fn parses_the_legacy_flat_layout() {
        let text = r#"
"LibraryFolders"
{
	"TimeNextStatsReport"		"1234567890"
	"ContentStatsID"		"-1234"
	"1"		"/run/media/alice/Games/SteamLibrary"
}
"#;
        let root = parse(text).unwrap();
        let folders = root.get("libraryfolders").unwrap();
        assert_eq!(
            folders.get("1").unwrap().as_str(),
            Some("/run/media/alice/Games/SteamLibrary")
        );
    }

    #[test]
    fn keys_are_case_insensitive() {
        let root = parse(r#""Root" { "PaTh" "/x" }"#).unwrap();
        assert_eq!(
            root.get("root").unwrap().get("path").unwrap().as_str(),
            Some("/x")
        );
    }

    #[test]
    fn keeps_duplicate_keys_in_order() {
        let root = parse(r#""r" { "k" "1" "k" "2" }"#).unwrap();
        let entries = root.get("r").unwrap().entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].1.as_str(), Some("2"));
    }

    #[test]
    fn handles_escapes_and_comments() {
        let text = "\"r\"\n{\n\t// a comment\n\t\"p\" \"/a\\\\b\\\"c\"\n}\n";
        let root = parse(text).unwrap();
        assert_eq!(
            root.get("r").unwrap().get("p").unwrap().as_str(),
            Some("/a\\b\"c")
        );
    }

    #[test]
    fn handles_non_ascii_paths() {
        let root = parse("\"r\" { \"path\" \"/mnt/Spiele-Bibliothèque/日本\" }").unwrap();
        assert_eq!(
            root.get("r").unwrap().get("path").unwrap().as_str(),
            Some("/mnt/Spiele-Bibliothèque/日本")
        );
    }

    #[test]
    fn rejects_truncated_and_unbalanced_input() {
        assert!(parse("\"r\" { \"p\" \"/a\"").is_err());
        assert!(parse("\"r\" { \"p\" \"/a").is_err());
        assert!(parse("}").is_err());
    }

    #[test]
    fn rejects_runaway_nesting() {
        let deep = format!("{}{}", "\"k\" {".repeat(200), "}".repeat(200));
        assert!(parse(&deep).is_err());
    }
}
