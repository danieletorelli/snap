//! Strict JSON reading and canonical writing (SPEC §4.1).
//!
//! SPEC §4.1 demands more strictness than a general-purpose parser gives:
//! duplicate object keys are an error (not last-wins), numbers must be
//! integers, and unknown fields are rejected. It also demands a *canonical*
//! writer, because `trees_equal` in the acceptance suite compares whole
//! repositories byte-for-byte, so two repositories that converged by different
//! merge routes must serialize identically.
//!
//! Objects keep insertion order in a `Vec` rather than a map: repository
//! objects have at most five keys, linear scan beats hashing at that size, and
//! — decisively — a `Vec` has no iteration-order nondeterminism to leak into
//! the write path.

use crate::error::{self, Result};
use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Json {
    Null,
    Bool(bool),
    /// JSON numbers restricted to integers; SPEC §4.1 rejects the rest.
    Int(i64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// Look up a key. Objects here are tiny, so linear scan is the right tool.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Json::Null => "null",
            Json::Bool(_) => "boolean",
            Json::Int(_) => "number",
            Json::Str(_) => "string",
            Json::Arr(_) => "array",
            Json::Obj(_) => "object",
        }
    }

    /// Reject any key outside `allowed`, and report the first missing one.
    /// Together with the duplicate-key check in the parser this gives SPEC
    /// §4.1's "unknown fields ... are errors" and "unique object keys".
    pub fn exact_fields(&self, allowed: &[&str]) -> Result<()> {
        self.exact_fields_with(allowed, &|key| {
            error::invalid_json(&format!("unknown field: {key}"))
        })
    }

    /// As [`Json::exact_fields`], but the caller names the container so the
    /// error reads the way the acceptance suite pins it.
    pub fn exact_fields_with(
        &self,
        allowed: &[&str],
        unknown: &dyn Fn(&str) -> crate::error::Error,
    ) -> Result<()> {
        let Json::Obj(fields) = self else {
            return Err(error::invalid_json(&format!(
                "expected object, found {}",
                self.type_name()
            )));
        };
        for (key, _) in fields {
            if !allowed.contains(&key.as_str()) {
                return Err(unknown(key));
            }
        }
        for key in allowed {
            if !fields.iter().any(|(k, _)| k == key) {
                return Err(error::invalid_json(&format!("missing field: {key}")));
            }
        }
        Ok(())
    }
}

// -- Parsing ---------------------------------------------------------------

/// Parse a complete JSON document, rejecting trailing content.
pub fn parse(input: &str) -> Result<Json> {
    let mut p = Parser {
        bytes: input.as_bytes(),
        pos: 0,
    };
    p.skip_whitespace();
    let value = p.value()?;
    p.skip_whitespace();
    if p.pos != p.bytes.len() {
        return Err(error::invalid_json("trailing content"));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        // RFC 8259 whitespace only; NBSP and friends stay errors.
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<()> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(error::invalid_json(&format!(
                "expected {:?} at byte {}",
                byte as char, self.pos
            )))
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json> {
        if self.bytes[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(error::invalid_json(&format!(
                "unexpected token at byte {}",
                self.pos
            )))
        }
    }

    fn value(&mut self) -> Result<Json> {
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(error::invalid_json(&format!(
                "unexpected byte at {}",
                self.pos
            ))),
            None => Err(error::invalid_json("unexpected end of input")),
        }
    }

    fn object(&mut self) -> Result<Json> {
        self.expect(b'{')?;
        let mut fields: Vec<(String, Json)> = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Obj(fields));
        }
        loop {
            self.skip_whitespace();
            let key = self.string()?;
            // SPEC §4.1: "Valid input has unique object keys." Detecting this
            // is the whole reason for a hand-written reader; serde and most
            // JSON libraries silently take the last value.
            if fields.iter().any(|(k, _)| *k == key) {
                return Err(error::duplicate_json_key(&key));
            }
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            let value = self.value()?;
            fields.push((key, value));
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Obj(fields));
                }
                _ => return Err(error::invalid_json("expected ',' or '}'")),
            }
        }
    }

    fn array(&mut self) -> Result<Json> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value()?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Arr(items));
                }
                _ => return Err(error::invalid_json("expected ',' or ']'")),
            }
        }
    }

    fn number(&mut self) -> Result<Json> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        let int_start = self.pos;
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(error::invalid_json("malformed number")),
        }
        if self.pos == int_start {
            return Err(error::invalid_json("malformed number"));
        }
        // SPEC §4.1: "non-integer numbers ... are errors". A fraction or
        // exponent is rejected outright rather than rounded.
        if matches!(self.peek(), Some(b'.' | b'e' | b'E')) {
            return Err(error::not_positive_safe_integer("number"));
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| error::invalid_json("malformed number"))?;
        text.parse::<i64>()
            .map(Json::Int)
            .map_err(|_| error::not_positive_safe_integer("number"))
    }

    fn string(&mut self) -> Result<String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let byte = self
                .peek()
                .ok_or_else(|| error::invalid_json("unterminated string"))?;
            match byte {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    let esc = self
                        .peek()
                        .ok_or_else(|| error::invalid_json("unterminated escape"))?;
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        _ => return Err(error::invalid_json("invalid escape")),
                    }
                }
                // RFC 8259 forbids unescaped control characters in strings.
                0x00..=0x1f => return Err(error::invalid_json("unescaped control character")),
                _ => {
                    let rest = std::str::from_utf8(&self.bytes[self.pos..])
                        .map_err(|_| error::invalid_json("invalid UTF-8"))?;
                    let ch = rest.chars().next().expect("non-empty");
                    out.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
    }

    fn unicode_escape(&mut self) -> Result<char> {
        let high = self.hex4()?;
        // Surrogate pairs must be complete; a lone surrogate is not a char.
        if (0xd800..0xdc00).contains(&high) {
            if !self.bytes[self.pos..].starts_with(b"\\u") {
                return Err(error::invalid_json("lone high surrogate"));
            }
            self.pos += 2;
            let low = self.hex4()?;
            if !(0xdc00..0xe000).contains(&low) {
                return Err(error::invalid_json("invalid low surrogate"));
            }
            let combined = 0x1_0000 + ((high - 0xd800) << 10) + (low - 0xdc00);
            return char::from_u32(combined).ok_or_else(|| error::invalid_json("invalid escape"));
        }
        if (0xdc00..0xe000).contains(&high) {
            return Err(error::invalid_json("lone low surrogate"));
        }
        char::from_u32(high).ok_or_else(|| error::invalid_json("invalid escape"))
    }

    fn hex4(&mut self) -> Result<u32> {
        let end = self.pos + 4;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| error::invalid_json("truncated \\u escape"))?;
        let text =
            std::str::from_utf8(slice).map_err(|_| error::invalid_json("invalid \\u escape"))?;
        let value =
            u32::from_str_radix(text, 16).map_err(|_| error::invalid_json("invalid \\u escape"))?;
        self.pos = end;
        Ok(value)
    }
}

// -- Canonical writing -----------------------------------------------------

/// Serialize with two-space indentation and a trailing LF (SPEC §4.1).
///
/// Deterministic by construction: the input carries its own key order, so the
/// same typed value always produces the same bytes.
#[must_use]
pub fn to_canonical_string(value: &Json) -> String {
    let mut out = String::new();
    write_value(&mut out, value, 0);
    out.push('\n');
    out
}

fn write_value(out: &mut String, value: &Json, depth: usize) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Int(n) => out.push_str(&n.to_string()),
        Json::Str(s) => write_string(out, s),
        Json::Arr(items) if items.is_empty() => out.push_str("[]"),
        Json::Arr(items) => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                indent(out, depth + 1);
                write_value(out, item, depth + 1);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            indent(out, depth);
            out.push(']');
        }
        Json::Obj(fields) if fields.is_empty() => out.push_str("{}"),
        Json::Obj(fields) => {
            out.push_str("{\n");
            for (i, (key, item)) in fields.iter().enumerate() {
                indent(out, depth + 1);
                write_string(out, key);
                out.push_str(": ");
                write_value(out, item, depth + 1);
                if i + 1 < fields.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            indent(out, depth);
            out.push('}');
        }
    }
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_spec_example_shape() {
        let value = parse(r#"{"format":1,"frontier":[["alice@example.com",1]],"patches":[]}"#)
            .expect("valid");
        assert_eq!(value.get("format"), Some(&Json::Int(1)));
        assert!(matches!(value.get("patches"), Some(Json::Arr(v)) if v.is_empty()));
    }

    #[test]
    fn rejects_duplicate_object_keys() {
        // `15-repository-validation` feeds exactly this. Most JSON libraries
        // accept it with last-wins semantics; SPEC §4.1 does not.
        let err = parse(r#"{"format":1,"format":1,"frontier":[],"patches":[]}"#).unwrap_err();
        assert!(
            err.detail().contains("duplicate JSON key"),
            "{}",
            err.detail()
        );
    }

    #[test]
    fn rejects_non_integer_numbers() {
        for text in ["1.5", "1e3", "1E3", "1.0", "-0.5"] {
            assert!(parse(text).is_err(), "{text} should be rejected");
        }
    }

    #[test]
    fn rejects_malformed_documents() {
        for text in [
            "",
            "{",
            "}",
            "[",
            "[1,]",
            "{\"a\":}",
            "{\"a\" 1}",
            "{'a':1}",
            "01",
            "+1",
            ".5",
            "nul",
            "tru",
            "\"unterminated",
            "{\"a\":1} trailing",
            "[1 2]",
        ] {
            assert!(parse(text).is_err(), "{text:?} should be rejected");
        }
    }

    #[test]
    fn accepts_ordinary_whitespace_and_any_key_order() {
        // SPEC §4.1: "Readers accept ordinary JSON whitespace and object-key
        // order. The parsed typed value ... is authoritative."
        let a = parse("{\"a\":1,\"b\":2}").unwrap();
        let b = parse("  {\n\t\"b\" : 2 ,\r\n \"a\" : 1 }  ").unwrap();
        assert_eq!(a.get("a"), b.get("a"));
        assert_eq!(a.get("b"), b.get("b"));
    }

    #[test]
    fn rejects_unescaped_control_characters_in_strings() {
        assert!(parse("\"a\nb\"").is_err());
        assert!(parse("\"a\\nb\"").is_ok());
    }

    #[test]
    fn handles_escapes_and_surrogate_pairs() {
        assert_eq!(parse(r#""aAb""#).unwrap(), Json::Str("aAb".into()));
        assert_eq!(parse(r#""😀""#).unwrap(), Json::Str("\u{1f600}".into()));
        assert!(parse(r#""\ud83d""#).is_err(), "lone high surrogate");
        assert!(parse(r#""\ude00""#).is_err(), "lone low surrogate");
    }

    #[test]
    fn exact_fields_rejects_unknown_and_missing() {
        let value = parse(r#"{"a":1,"b":2}"#).unwrap();
        assert!(value.exact_fields(&["a", "b"]).is_ok());
        assert!(value.exact_fields(&["a"]).is_err(), "unknown field b");
        assert!(
            value.exact_fields(&["a", "b", "c"]).is_err(),
            "missing field c"
        );
    }

    #[test]
    fn writes_two_space_indentation_and_a_trailing_lf() {
        let value = parse(r#"{"format":1,"frontier":[],"patches":[[1,2]]}"#).unwrap();
        let text = to_canonical_string(&value);
        assert!(
            text.ends_with(":\n") || text.ends_with("}\n"),
            "trailing LF"
        );
        assert_eq!(
            text,
            "{\n  \"format\": 1,\n  \"frontier\": [],\n  \"patches\": [\n    [\n      1,\n      2\n    ]\n  ]\n}\n"
        );
    }

    #[test]
    fn writing_is_deterministic_and_reparses_identically() {
        let value = parse(r#"{"b":[1,2,{"z":"é","a":null}],"a":true}"#).unwrap();
        let once = to_canonical_string(&value);
        let twice = to_canonical_string(&parse(&once).unwrap());
        assert_eq!(once, twice, "round trip must be byte-stable");
    }

    #[test]
    fn escapes_control_characters_on_write() {
        let text = to_canonical_string(&Json::Str("a\tb\nc\u{1}d\"e\\f".into()));
        assert_eq!(text, "\"a\\tb\\nc\\u0001d\\\"e\\\\f\"\n");
    }
}
