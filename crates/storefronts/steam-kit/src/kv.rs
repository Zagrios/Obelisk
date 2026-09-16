//! Bounded readers for Steam's text VDF and binary KeyValue package metadata.

use std::str;

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
const MAX_ENTRIES: usize = 250_000;

#[derive(Debug, thiserror::Error)]
#[error("{}", crate::message::format("backend.steam.kv.invalid", &[.0.to_string()]))]
pub(crate) struct KvError(pub(crate) &'static str);

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Kv {
    Value(String),
    Object(Vec<(String, Kv)>),
}

impl Kv {
    pub(crate) fn get(&self, key: &str) -> Option<&Self> {
        self.children()
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value)
    }

    pub(crate) fn at(&self, path: &[&str]) -> Option<&Self> {
        path.iter().try_fold(self, |node, name| node.get(name))
    }

    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Self::Value(value) => Some(value),
            Self::Object(_) => None,
        }
    }

    pub(crate) fn children(&self) -> &[(String, Kv)] {
        match self {
            Self::Object(children) => children,
            Self::Value(_) => &[],
        }
    }
}

pub(crate) fn text(bytes: &[u8]) -> Result<Kv, KvError> {
    if bytes.len() > MAX_BYTES {
        return Err(KvError("backend.steam.kv.documentTooLarge"));
    }
    let source = str::from_utf8(bytes).map_err(|_| KvError("backend.steam.kv.invalidUtf8"))?;
    let source = source.trim_end_matches('\0').trim_start_matches('\u{feff}');
    let mut parser = TextParser {
        source,
        offset: 0,
        entries: 0,
    };
    parser.object(0, false)
}

struct TextParser<'a> {
    source: &'a str,
    offset: usize,
    entries: usize,
}

#[derive(Debug, PartialEq)]
enum Token {
    Word(String),
    Open,
    Close,
}

impl TextParser<'_> {
    fn object(&mut self, depth: usize, nested: bool) -> Result<Kv, KvError> {
        if depth > MAX_DEPTH {
            return Err(KvError("backend.steam.kv.excessiveNesting"));
        }
        let mut fields = Vec::new();
        loop {
            let key = match self.token()? {
                Some(Token::Word(key)) => key,
                Some(Token::Close) if nested => return Ok(Kv::Object(fields)),
                None if !nested => return Ok(Kv::Object(fields)),
                _ => return Err(KvError("backend.steam.kv.missingKeyOrBrace")),
            };
            self.entries += 1;
            if self.entries > MAX_ENTRIES {
                return Err(KvError("backend.steam.kv.tooManyFields"));
            }
            let value = match self.token()? {
                Some(Token::Word(value)) => Kv::Value(value),
                Some(Token::Open) => self.object(depth + 1, true)?,
                _ => return Err(KvError("backend.steam.kv.missingValue")),
            };
            fields.push((key, value));
        }
    }

    fn token(&mut self) -> Result<Option<Token>, KvError> {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.next();
            }
            if self.source[self.offset..].starts_with("//") {
                while self.next().is_some_and(|ch| ch != '\n') {}
            } else {
                break;
            }
        }
        let Some(first) = self.next() else {
            return Ok(None);
        };
        match first {
            '{' => Ok(Some(Token::Open)),
            '}' => Ok(Some(Token::Close)),
            '"' => {
                let mut value = String::new();
                loop {
                    match self.next() {
                        Some('"') => return Ok(Some(Token::Word(value))),
                        Some('\\') => match self.next() {
                            Some('n') => value.push('\n'),
                            Some('r') => value.push('\r'),
                            Some('t') => value.push('\t'),
                            Some('"') => value.push('"'),
                            Some('\\') => value.push('\\'),
                            Some(other) => {
                                value.push('\\');
                                value.push(other);
                            }
                            None => return Err(KvError("backend.steam.kv.truncatedEscape")),
                        },
                        Some(ch) => value.push(ch),
                        None => return Err(KvError("backend.steam.kv.unterminatedString")),
                    }
                }
            }
            _ => {
                let mut word = String::from(first);
                while self
                    .peek()
                    .is_some_and(|ch| !ch.is_whitespace() && ch != '{' && ch != '}')
                {
                    if let Some(ch) = self.next() {
                        word.push(ch);
                    }
                }
                Ok(Some(Token::Word(word)))
            }
        }
    }

    fn peek(&self) -> Option<char> {
        self.source[self.offset..].chars().next()
    }

    fn next(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.offset += ch.len_utf8();
        Some(ch)
    }
}

pub(crate) fn package(bytes: &[u8]) -> Result<Kv, KvError> {
    if bytes.len() > MAX_BYTES {
        return Err(KvError("backend.steam.kv.documentTooLarge"));
    }
    let mut parser = BinaryParser {
        remaining: bytes,
        entries: 0,
    };
    if u32::from_le_bytes(parser.take::<4>()?) != 1 {
        return Err(KvError("backend.steam.kv.unknownPackageVersion"));
    }
    let value = parser.object(0)?;
    if !parser.remaining.is_empty() {
        return Err(KvError("backend.steam.kv.trailingBytes"));
    }
    Ok(value)
}

struct BinaryParser<'a> {
    remaining: &'a [u8],
    entries: usize,
}

impl BinaryParser<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], KvError> {
        let (head, tail) = self
            .remaining
            .split_at_checked(N)
            .ok_or(KvError("backend.steam.kv.truncatedPackage"))?;
        self.remaining = tail;
        head.try_into()
            .map_err(|_| KvError("backend.steam.kv.truncatedPackage"))
    }

    fn string(&mut self) -> Result<String, KvError> {
        let length = self
            .remaining
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(KvError("backend.steam.kv.truncatedBinaryString"))?;
        let value = str::from_utf8(&self.remaining[..length])
            .map_err(|_| KvError("backend.steam.kv.invalidUtf8"))?
            .to_owned();
        self.remaining = &self.remaining[length + 1..];
        Ok(value)
    }

    fn object(&mut self, depth: usize) -> Result<Kv, KvError> {
        if depth > MAX_DEPTH {
            return Err(KvError("backend.steam.kv.excessiveNesting"));
        }
        let mut fields = Vec::new();
        loop {
            let kind = self.take::<1>()?[0];
            if kind == 8 || kind == 11 {
                return Ok(Kv::Object(fields));
            }
            self.entries += 1;
            if self.entries > MAX_ENTRIES {
                return Err(KvError("backend.steam.kv.tooManyFields"));
            }
            let name = self.string()?;
            let value = match kind {
                0 => self.object(depth + 1)?,
                1 => Kv::Value(self.string()?),
                2 | 4 | 6 => Kv::Value(i32::from_le_bytes(self.take::<4>()?).to_string()),
                3 => Kv::Value(f32::from_le_bytes(self.take::<4>()?).to_string()),
                7 => Kv::Value(u64::from_le_bytes(self.take::<8>()?).to_string()),
                10 => Kv::Value(i64::from_le_bytes(self.take::<8>()?).to_string()),
                _ => return Err(KvError("backend.steam.kv.unsupportedBinaryType")),
            };
            fields.push((name, value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested_text_comments_escapes_and_unicode() {
        let parsed = text(
            br#"// generated
            "appinfo" { "appid" "42" "config" { "installdir" "A\\B" } }
        "#,
        )
        .unwrap();
        assert_eq!(
            parsed.at(&["appinfo", "appid"]).and_then(Kv::text),
            Some("42")
        );
        assert_eq!(
            parsed
                .at(&["appinfo", "config", "installdir"])
                .and_then(Kv::text),
            Some("A\\B")
        );
        assert_eq!(
            text("key \"Star ☆\"".as_bytes())
                .unwrap()
                .get("key")
                .and_then(Kv::text),
            Some("Star ☆")
        );
    }

    #[test]
    fn rejects_incomplete_text_and_unmatched_braces() {
        for input in [b"a {".as_slice(), b"a", b"a \"b", b"}", b"a { b }"] {
            assert!(text(input).is_err());
        }
    }

    #[test]
    fn decodes_package_app_ids_and_rejects_truncation() {
        let mut bytes = 1_u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"\0" /* object */);
        bytes.extend_from_slice(b"123\0\0appids\0\x020\0");
        bytes.extend_from_slice(&42_i32.to_le_bytes());
        bytes.extend_from_slice(&[8, 8, 8]);
        let parsed = package(&bytes).unwrap();
        assert_eq!(
            parsed.at(&["123", "appids", "0"]).and_then(Kv::text),
            Some("42")
        );
        for length in 0..bytes.len() {
            assert!(package(&bytes[..length]).is_err());
        }
    }
}
