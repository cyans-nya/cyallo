//! Minimal JSON reader for trace files. It accepts the documents written by
//! the report module (any valid JSON, with a nesting limit) and reports the
//! byte offset of the first problem.
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub(crate) fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(members) => members
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }
    pub(crate) fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Number(number) => Some(*number),
            _ => None,
        }
    }
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(text) => Some(text),
            _ => None,
        }
    }
    pub(crate) fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }
}

/// A syntax error, with the byte offset where it was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParseError {
    pub(crate) offset: usize,
    pub(crate) reason: &'static str,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid JSON at byte {}: {}", self.offset, self.reason)
    }
}

impl std::error::Error for ParseError {}

/// Deeper nesting than any trace we write; guards the recursion.
const MAX_DEPTH: usize = 64;

pub(crate) fn parse(text: &str) -> Result<Json, ParseError> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        pos: 0,
        depth: 0,
    };
    parser.skip_ws();
    let value = parser.value()?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return Err(parser.error("trailing characters after the document"));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    fn error(&self, reason: &'static str) -> ParseError {
        ParseError {
            offset: self.pos,
            reason,
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.bytes.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.bytes.get(self.pos) == Some(&byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn literal(&mut self, word: &[u8], value: Json) -> Result<Json, ParseError> {
        if self.bytes[self.pos..].starts_with(word) {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(self.error("unknown literal"))
        }
    }

    fn value(&mut self) -> Result<Json, ParseError> {
        match self.bytes.get(self.pos) {
            None => Err(self.error("unexpected end of input")),
            Some(b'{') => self.nested(Self::object),
            Some(b'[') => self.nested(Self::array),
            Some(b'"') => self.string().map(Json::String),
            Some(b't') => self.literal(b"true", Json::Bool(true)),
            Some(b'f') => self.literal(b"false", Json::Bool(false)),
            Some(b'n') => self.literal(b"null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("unexpected character")),
        }
    }

    fn nested(
        &mut self,
        parse: fn(&mut Self) -> Result<Json, ParseError>,
    ) -> Result<Json, ParseError> {
        if self.depth >= MAX_DEPTH {
            return Err(self.error("nesting is too deep"));
        }
        self.depth += 1;
        let value = parse(self);
        self.depth -= 1;
        value
    }

    fn object(&mut self) -> Result<Json, ParseError> {
        self.pos += 1;
        let mut members = Vec::new();
        self.skip_ws();
        if self.eat(b'}') {
            return Ok(Json::Object(members));
        }
        loop {
            self.skip_ws();
            if self.bytes.get(self.pos) != Some(&b'"') {
                return Err(self.error("expected an object key"));
            }
            let key = self.string()?;
            self.skip_ws();
            if !self.eat(b':') {
                return Err(self.error("expected ':' after an object key"));
            }
            self.skip_ws();
            let value = self.value()?;
            members.push((key, value));
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            if self.eat(b'}') {
                return Ok(Json::Object(members));
            }
            return Err(self.error("expected ',' or '}' in an object"));
        }
    }

    fn array(&mut self) -> Result<Json, ParseError> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.eat(b']') {
            return Ok(Json::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value()?);
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            if self.eat(b']') {
                return Ok(Json::Array(items));
            }
            return Err(self.error("expected ',' or ']' in an array"));
        }
    }

    fn string(&mut self) -> Result<String, ParseError> {
        // The caller has checked for the opening quote.
        self.pos += 1;
        let mut out = String::new();
        loop {
            let start = self.pos;
            while let Some(&byte) = self.bytes.get(self.pos) {
                if byte == b'"' || byte == b'\\' || byte < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            // The input is a `str`, and the run stops at ASCII delimiters only,
            // so the slice is valid UTF-8.
            let run = std::str::from_utf8(&self.bytes[start..self.pos])
                .map_err(|_| self.error("invalid UTF-8"))?;
            out.push_str(run);
            match self.bytes.get(self.pos) {
                None => return Err(self.error("unterminated string")),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    out.push(self.escape()?);
                }
                Some(_) => return Err(self.error("control character in a string")),
            }
        }
    }

    fn escape(&mut self) -> Result<char, ParseError> {
        let Some(&byte) = self.bytes.get(self.pos) else {
            return Err(self.error("unterminated escape"));
        };
        self.pos += 1;
        Ok(match byte {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let high = self.hex4()?;
                if (0xD800..0xDC00).contains(&high) {
                    if self.bytes.get(self.pos..self.pos + 2) != Some(&b"\\u"[..]) {
                        return Err(self.error("unpaired surrogate"));
                    }
                    self.pos += 2;
                    let low = self.hex4()?;
                    if !(0xDC00..0xE000).contains(&low) {
                        return Err(self.error("invalid surrogate pair"));
                    }
                    let code = 0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00);
                    char::from_u32(code).ok_or_else(|| self.error("invalid code point"))?
                } else {
                    char::from_u32(high).ok_or_else(|| self.error("invalid code point"))?
                }
            }
            _ => return Err(self.error("invalid escape sequence")),
        })
    }

    fn hex4(&mut self) -> Result<u32, ParseError> {
        let digits = self
            .bytes
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| self.error("short unicode escape"))?;
        let text = std::str::from_utf8(digits).map_err(|_| self.error("invalid unicode escape"))?;
        let value =
            u32::from_str_radix(text, 16).map_err(|_| self.error("invalid unicode escape"))?;
        self.pos += 4;
        Ok(value)
    }

    fn number(&mut self) -> Result<Json, ParseError> {
        let start = self.pos;
        self.eat(b'-');
        while matches!(
            self.bytes.get(self.pos),
            Some(b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
        ) {
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).map_err(|_| ParseError {
            offset: start,
            reason: "invalid number",
        })?;
        text.parse::<f64>()
            .map(Json::Number)
            .map_err(|_| ParseError {
                offset: start,
                reason: "invalid number",
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_documents_the_report_writes() {
        let value = parse(r#"{"a":[1,-2.5e1,true,null],"b":{"c":"d\né"}}"#).unwrap();
        assert_eq!(
            value.get("a").unwrap().as_array().unwrap()[1],
            Json::Number(-25.0)
        );
        assert_eq!(
            value.get("b").unwrap().get("c").unwrap().as_str(),
            Some("d\né")
        );
    }

    #[test]
    fn decodes_surrogate_pairs_and_rejects_lone_surrogates() {
        assert_eq!(parse(r#""😀""#).unwrap(), Json::String("😀".to_owned()));
        assert!(parse(r#""\ud83d""#).is_err());
    }

    #[test]
    fn reports_offsets_for_malformed_input() {
        let error = parse(r#"{"a" 1}"#).unwrap_err();
        assert_eq!(error.offset, 5);
        assert_eq!(
            parse("[1,2] x").unwrap_err().reason,
            "trailing characters after the document"
        );
        assert_eq!(parse(r#""open"#).unwrap_err().reason, "unterminated string");
        assert_eq!(parse("").unwrap_err().reason, "unexpected end of input");
    }

    #[test]
    fn limits_nesting() {
        let deep = "[".repeat(MAX_DEPTH + 1) + &"]".repeat(MAX_DEPTH + 1);
        assert_eq!(parse(&deep).unwrap_err().reason, "nesting is too deep");
        let fine = "[".repeat(MAX_DEPTH) + &"]".repeat(MAX_DEPTH);
        assert!(parse(&fine).is_ok());
    }
}
