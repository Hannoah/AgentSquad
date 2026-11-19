//! Minimal zero-dependency RFC 8259 JSON parser and pretty-printer that
//! preserves object key insertion order so existing config files stay tidy.

use std::fmt::Write;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    pub fn empty_object() -> Self {
        JsonValue::Object(Vec::new())
    }

    pub fn as_object(&self) -> Option<&[(String, JsonValue)]> {
        match self {
            JsonValue::Object(entries) => Some(entries),
            _ => None,
        }
    }

    pub fn as_object_mut(&mut self) -> Option<&mut Vec<(String, JsonValue)>> {
        match self {
            JsonValue::Object(entries) => Some(entries),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        self.as_object()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    pub fn set(&mut self, key: impl Into<String>, value: JsonValue) -> bool {
        let key = key.into();
        let Some(entries) = self.as_object_mut() else {
            return false;
        };
        if let Some((_, existing)) = entries.iter_mut().find(|(k, _)| k == &key) {
            *existing = value;
        } else {
            entries.push((key, value));
        }
        true
    }

    pub fn remove(&mut self, key: &str) -> Option<JsonValue> {
        let entries = self.as_object_mut()?;
        let pos = entries.iter().position(|(k, _)| k == key)?;
        Some(entries.remove(pos).1)
    }

    pub fn to_pretty(&self) -> String {
        let mut out = String::new();
        write_pretty(self, 0, &mut out);
        out.push('\n');
        out
    }
}

pub fn parse(input: &str) -> Result<JsonValue, String> {
    let trimmed = input.strip_prefix('\u{feff}').unwrap_or(input);
    let mut p = Parser {
        bytes: trimmed.as_bytes(),
        pos: 0,
    };
    p.skip_ws();
    let val = p.parse_value(0)?;
    p.skip_ws();
    if p.pos < p.bytes.len() {
        return Err(format!("Unexpected trailing character at byte {}", p.pos));
    }
    Ok(val)
}

const MAX_DEPTH: usize = 128;

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.pos += 1;
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<JsonValue, String> {
        if depth > MAX_DEPTH {
            return Err("JSON nesting depth exceeded".into());
        }
        self.skip_ws();
        match self.peek() {
            Some(b'n') => self.expect_literal(b"null").map(|()| JsonValue::Null),
            Some(b't') => self.expect_literal(b"true").map(|()| JsonValue::Bool(true)),
            Some(b'f') => self
                .expect_literal(b"false")
                .map(|()| JsonValue::Bool(false)),
            Some(b'"') => self.parse_string().map(JsonValue::String),
            Some(b'[') => self.parse_array(depth + 1),
            Some(b'{') => self.parse_object(depth + 1),
            Some(b'-' | b'0'..=b'9') => self.parse_number().map(JsonValue::Number),
            Some(c) => Err(format!(
                "Unexpected byte '{}' at byte {}",
                c as char, self.pos
            )),
            None => Err("Unexpected end of JSON input".into()),
        }
    }

    fn expect_literal(&mut self, lit: &[u8]) -> Result<(), String> {
        if self.bytes.get(self.pos..self.pos + lit.len()) == Some(lit) {
            self.pos += lit.len();
            Ok(())
        } else {
            Err(format!("Invalid literal at byte {}", self.pos))
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        if self.next() != Some(b'"') {
            return Err(format!("Expected '\"' at byte {}", self.pos));
        }
        let mut out = String::new();
        loop {
            let start = self.pos;
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            if self.pos > start {
                let slice = std::str::from_utf8(&self.bytes[start..self.pos])
                    .map_err(|_| "Invalid UTF-8 in JSON string".to_string())?;
                out.push_str(slice);
            }
            match self.next() {
                Some(b'"') => return Ok(out),
                Some(b'\\') => {
                    let esc = self
                        .next()
                        .ok_or_else(|| "Unterminated escape in JSON string".to_string())?;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\x08'),
                        b'f' => out.push('\x0c'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let code = self.parse_hex4()?;
                            if (0xD800..=0xDBFF).contains(&code) {
                                if self.bytes.get(self.pos..self.pos + 2) == Some(b"\\u") {
                                    self.pos += 2;
                                    let low = self.parse_hex4()?;
                                    if !(0xDC00..=0xDFFF).contains(&low) {
                                        return Err("Invalid UTF-16 surrogate pair".into());
                                    }
                                    let combined = 0x10000
                                        + (((u32::from(code) - 0xD800) << 10)
                                            | (u32::from(low) - 0xDC00));
                                    let ch = char::from_u32(combined)
                                        .ok_or_else(|| "Invalid Unicode scalar".to_string())?;
                                    out.push(ch);
                                } else {
                                    return Err("Unpaired UTF-16 high surrogate".into());
                                }
                            } else if (0xDC00..=0xDFFF).contains(&code) {
                                return Err("Unexpected UTF-16 low surrogate".into());
                            } else {
                                let ch = char::from_u32(u32::from(code))
                                    .ok_or_else(|| "Invalid Unicode scalar".to_string())?;
                                out.push(ch);
                            }
                        }
                        other => return Err(format!("Invalid escape '\\{}'", other as char)),
                    }
                }
                Some(ctrl) => return Err(format!("Unescaped control byte 0x{ctrl:02x} in string")),
                None => return Err("Unterminated JSON string".into()),
            }
        }
    }

    fn parse_hex4(&mut self) -> Result<u16, String> {
        let mut val: u16 = 0;
        for _ in 0..4 {
            let b = self
                .next()
                .ok_or_else(|| "Truncated \\u escape in JSON string".to_string())?;
            let d = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => return Err(format!("Invalid hex digit '{}' in \\u escape", b as char)),
            };
            val = (val << 4) | u16::from(d);
        }
        Ok(val)
    }

    fn parse_number(&mut self) -> Result<String, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(format!("Invalid number at byte {}", self.pos)),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(format!("Expected fraction digits at byte {}", self.pos));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(format!("Expected exponent digits at byte {}", self.pos));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let s = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| "Invalid UTF-8 in JSON number".to_string())?;
        Ok(s.to_string())
    }

    fn parse_array(&mut self, depth: usize) -> Result<JsonValue, String> {
        self.pos += 1; // '['
        self.skip_ws();
        let mut items = Vec::new();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(JsonValue::Array(items));
        }
        loop {
            items.push(self.parse_value(depth)?);
            self.skip_ws();
            match self.next() {
                Some(b',') => {}
                Some(b']') => return Ok(JsonValue::Array(items)),
                Some(c) => {
                    return Err(format!(
                        "Expected ',' or ']' at byte {}, got '{}'",
                        self.pos - 1,
                        c as char
                    ))
                }
                None => return Err("Unterminated JSON array".into()),
            }
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<JsonValue, String> {
        self.pos += 1; // '{'
        self.skip_ws();
        let mut entries: Vec<(String, JsonValue)> = Vec::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(JsonValue::Object(entries));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(format!("Expected string key at byte {}", self.pos));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if self.next() != Some(b':') {
                return Err(format!("Expected ':' after key at byte {}", self.pos));
            }
            let val = self.parse_value(depth)?;
            if let Some((_, existing)) = entries.iter_mut().find(|(k, _)| k == &key) {
                *existing = val;
            } else {
                entries.push((key, val));
            }
            self.skip_ws();
            match self.next() {
                Some(b',') => {}
                Some(b'}') => return Ok(JsonValue::Object(entries)),
                Some(c) => {
                    return Err(format!(
                        "Expected ',' or '}}' at byte {}, got '{}'",
                        self.pos - 1,
                        c as char
                    ))
                }
                None => return Err("Unterminated JSON object".into()),
            }
        }
    }
}

fn write_pretty(val: &JsonValue, indent: usize, out: &mut String) {
    match val {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Bool(true) => out.push_str("true"),
        JsonValue::Bool(false) => out.push_str("false"),
        JsonValue::Number(n) => out.push_str(n),
        JsonValue::String(s) => write_json_str(s, out),
        JsonValue::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                push_indent(indent + 2, out);
                write_pretty(item, indent + 2, out);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(indent, out);
            out.push(']');
        }
        JsonValue::Object(entries) => {
            if entries.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (i, (k, v)) in entries.iter().enumerate() {
                push_indent(indent + 2, out);
                write_json_str(k, out);
                out.push_str(": ");
                write_pretty(v, indent + 2, out);
                if i + 1 < entries.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(indent, out);
            out.push('}');
        }
    }
}

fn push_indent(n: usize, out: &mut String) {
    for _ in 0..n {
        out.push(' ');
    }
}

fn write_json_str(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}
