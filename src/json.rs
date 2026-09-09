//! A small, bounded JSON reader.
//!
//! Needed to read GOG's `goggame-*.info` files and the output of
//! `lutris --list-games --json`. Both are untrusted input from disk or from a
//! subprocess, so the parser is depth- and size-limited like the VDF one.

use std::collections::BTreeMap;

const MAX_DEPTH: usize = 32;
const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(map) => map.get(key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> &[Json] {
        match self {
            Json::Array(items) => items,
            _ => &[],
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// String value of a key, for the common "read one field" case.
    pub fn string(&self, key: &str) -> Option<String> {
        self.get(key).and_then(|v| v.as_str()).map(str::to_string)
    }
}

pub fn parse(input: &str) -> Result<Json, String> {
    if input.len() > MAX_BYTES {
        return Err(format!("input is {} bytes, over the limit", input.len()));
    }
    let mut parser = Parser {
        bytes: input.as_bytes(),
        pos: 0,
    };
    parser.skip_whitespace();
    let value = parser.value(0)?;
    parser.skip_whitespace();
    if parser.pos < parser.bytes.len() {
        return Err(format!("trailing content at byte {}", parser.pos));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b) if b.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at byte {}", byte as char, self.pos))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return Err("nesting is deeper than the parser allows".to_string());
        }
        self.skip_whitespace();
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => Ok(Json::String(self.string()?)),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(_) => self.number(),
            None => Err("input ended early".to_string()),
        }
    }

    fn literal(&mut self, text: &str, value: Json) -> Result<Json, String> {
        if self.bytes[self.pos..].starts_with(text.as_bytes()) {
            self.pos += text.len();
            Ok(value)
        } else {
            Err(format!("unrecognised literal at byte {}", self.pos))
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, String> {
        self.expect(b'{')?;
        let mut map = BTreeMap::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Object(map));
        }
        loop {
            self.skip_whitespace();
            let key = self.string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            let value = self.value(depth + 1)?;
            map.insert(key, value);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Object(map));
                }
                _ => return Err(format!("expected ',' or '}}' at byte {}", self.pos)),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Array(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Array(items));
                }
                _ => return Err(format!("expected ',' or ']' at byte {}", self.pos)),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let start = self.pos;
        let mut out = String::new();
        let mut plain = true;
        loop {
            let byte = self.peek().ok_or("unterminated string")?;
            match byte {
                b'"' => {
                    if plain {
                        out = String::from_utf8_lossy(&self.bytes[start..self.pos]).into_owned();
                    }
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    if plain {
                        out = String::from_utf8_lossy(&self.bytes[start..self.pos]).into_owned();
                        plain = false;
                    }
                    self.pos += 1;
                    let escaped = self.peek().ok_or("unterminated escape")?;
                    self.pos += 1;
                    match escaped {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'u' => {
                            let hex = self
                                .bytes
                                .get(self.pos..self.pos + 4)
                                .ok_or("truncated \\u escape")?;
                            let code = u32::from_str_radix(
                                std::str::from_utf8(hex).map_err(|_| "bad \\u escape")?,
                                16,
                            )
                            .map_err(|_| "bad \\u escape")?;
                            self.pos += 4;
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        other => return Err(format!("unknown escape '\\{}'", other as char)),
                    }
                }
                _ => {
                    if !plain {
                        let text = String::from_utf8_lossy(&self.bytes[self.pos..self.pos + 1]);
                        out.push_str(&text);
                    }
                    self.pos += 1;
                }
            }
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        while matches!(self.peek(), Some(b) if b.is_ascii_digit()
            || b == b'-' || b == b'+' || b == b'.' || b == b'e' || b == b'E')
        {
            self.pos += 1;
        }
        let text = &self.bytes[start..self.pos];
        std::str::from_utf8(text)
            .ok()
            .and_then(|t| t.parse::<f64>().ok())
            .map(Json::Number)
            .ok_or_else(|| format!("bad number at byte {start}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_goggame_info_file() {
        let text = r#"{
            "buildId": "55",
            "name": "Beneath a Steel Sky",
            "playTasks": [
                {"category": "game", "isPrimary": true, "path": "BASS.exe", "type": "FileTask"},
                {"category": "document", "path": "manual.pdf", "type": "FileTask"}
            ]
        }"#;
        let value = parse(text).unwrap();
        assert_eq!(value.string("name").as_deref(), Some("Beneath a Steel Sky"));
        let primary = value
            .get("playTasks")
            .unwrap()
            .as_array()
            .iter()
            .find(|t| t.get("isPrimary").and_then(Json::as_bool) == Some(true))
            .unwrap();
        assert_eq!(primary.string("path").as_deref(), Some("BASS.exe"));
    }

    #[test]
    fn handles_escapes_and_unicode() {
        let value = parse(r#"{"a": "line\nbreak", "b": "quote\"inside", "c": "été"}"#).unwrap();
        assert_eq!(value.string("a").as_deref(), Some("line\nbreak"));
        assert_eq!(value.string("b").as_deref(), Some("quote\"inside"));
        assert_eq!(value.string("c").as_deref(), Some("été"));
    }

    #[test]
    fn handles_numbers_arrays_and_nulls() {
        let value = parse(r#"{"n": -12.5, "e": 1e3, "list": [1, 2, 3], "nothing": null}"#).unwrap();
        assert_eq!(value.get("n"), Some(&Json::Number(-12.5)));
        assert_eq!(value.get("e"), Some(&Json::Number(1000.0)));
        assert_eq!(value.get("list").unwrap().as_array().len(), 3);
        assert_eq!(value.get("nothing"), Some(&Json::Null));
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(parse("{").is_err());
        assert!(parse(r#"{"a": }"#).is_err());
        assert!(parse(r#"{"a": 1} trailing"#).is_err());
        assert!(parse(&format!("{}{}", "[".repeat(100), "]".repeat(100))).is_err());
    }
}
