//! Bounded STOMP 1.2 framing. Session negotiation belongs to the caller.
//! Wire rules: <https://stomp.github.io/stomp-specification-1.2.html>.

use std::collections::BTreeMap;

pub(crate) const MAX_FRAME_BYTES: usize = 1024 * 1024;
// Command, header lines and their terminating blank line share this budget.
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_HEADERS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Frame {
    pub command: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtocolError {
    MalformedFrame,
    InvalidUtf8,
    InvalidEscape,
    LimitExceeded,
    Terminal,
}

#[derive(Default)]
enum Section {
    #[default]
    Command,
    Headers,
    Body,
}

#[derive(Default)]
pub(crate) struct Decoder {
    section: Section,
    line: Vec<u8>,
    command: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
    content_length: Option<usize>,
    terminal: bool,
    frame_bytes: usize,
    header_bytes: usize,
    header_count: usize,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Frame>, ProtocolError> {
        if self.terminal {
            return Err(ProtocolError::Terminal);
        }
        let result = self.push_inner(bytes);
        if result.is_err() {
            *self = Self {
                terminal: true,
                ..Self::default()
            };
        }
        result
    }

    fn push_inner(&mut self, bytes: &[u8]) -> Result<Vec<Frame>, ProtocolError> {
        let mut frames = Vec::new();
        for &byte in bytes {
            self.frame_bytes += 1;
            if self.frame_bytes > MAX_FRAME_BYTES {
                return Err(ProtocolError::LimitExceeded);
            }
            match self.section {
                Section::Command | Section::Headers => {
                    self.header_bytes += 1;
                    if self.header_bytes > MAX_HEADER_BYTES {
                        return Err(ProtocolError::LimitExceeded);
                    }
                    if byte != b'\n' {
                        if byte == 0 {
                            return Err(ProtocolError::MalformedFrame);
                        }
                        self.line.push(byte);
                        continue;
                    }
                    let line = std::mem::take(&mut self.line);
                    let line =
                        std::str::from_utf8(&line).map_err(|_| ProtocolError::InvalidUtf8)?;
                    let line = line.strip_suffix('\r').unwrap_or(line);
                    match self.section {
                        Section::Command if line.is_empty() => {
                            self.frame_bytes = 0;
                            self.header_bytes = 0;
                        }
                        Section::Command => {
                            validate_command(line)?;
                            self.command = line.to_owned();
                            self.section = Section::Headers;
                        }
                        Section::Headers if line.is_empty() => {
                            self.content_length = self
                                .headers
                                .get("content-length")
                                .map(|length| parse_content_length(length))
                                .transpose()?;
                            if let Some(length) = self.content_length
                                && length > MAX_FRAME_BYTES - self.frame_bytes - 1
                            {
                                return Err(ProtocolError::LimitExceeded);
                            }
                            self.section = Section::Body;
                        }
                        Section::Headers => {
                            self.header_count += 1;
                            if self.header_count > MAX_HEADERS {
                                return Err(ProtocolError::LimitExceeded);
                            }
                            let (key, value) =
                                line.split_once(':').ok_or(ProtocolError::MalformedFrame)?;
                            if key.is_empty() {
                                return Err(ProtocolError::MalformedFrame);
                            }
                            let escaped = !matches!(self.command.as_str(), "CONNECT" | "CONNECTED");
                            let key = decode_header(key, escaped)?;
                            let value = decode_header(value, escaped)?;
                            self.headers.entry(key).or_insert(value);
                        }
                        Section::Body => unreachable!(),
                    }
                }
                Section::Body
                    if self
                        .content_length
                        .map_or(byte == 0, |length| self.body.len() == length) =>
                {
                    if byte != 0 {
                        return Err(ProtocolError::MalformedFrame);
                    }
                    frames.push(Frame {
                        command: std::mem::take(&mut self.command),
                        headers: std::mem::take(&mut self.headers),
                        body: std::mem::take(&mut self.body),
                    });
                    self.section = Section::Command;
                    self.content_length = None;
                    self.frame_bytes = 0;
                    self.header_bytes = 0;
                    self.header_count = 0;
                }
                Section::Body => {
                    if !allows_body(&self.command) {
                        return Err(ProtocolError::MalformedFrame);
                    }
                    self.body.push(byte);
                }
            }
        }
        Ok(frames)
    }
}

fn validate_command(command: &str) -> Result<(), ProtocolError> {
    if matches!(
        command,
        "CONNECT"
            | "STOMP"
            | "CONNECTED"
            | "SEND"
            | "SUBSCRIBE"
            | "UNSUBSCRIBE"
            | "ACK"
            | "NACK"
            | "BEGIN"
            | "COMMIT"
            | "ABORT"
            | "DISCONNECT"
            | "MESSAGE"
            | "RECEIPT"
            | "ERROR"
    ) {
        Ok(())
    } else {
        Err(ProtocolError::MalformedFrame)
    }
}

fn allows_body(command: &str) -> bool {
    matches!(command, "SEND" | "MESSAGE" | "ERROR")
}

fn parse_content_length(value: &str) -> Result<usize, ProtocolError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ProtocolError::MalformedFrame);
    }
    value.parse().map_err(|_| ProtocolError::LimitExceeded)
}

fn decode_header(value: &str, escaped: bool) -> Result<String, ProtocolError> {
    if value.contains(['\r', '\n', '\0']) || (escaped && value.contains(':')) {
        return Err(ProtocolError::MalformedFrame);
    }
    if !escaped {
        return Ok(value.to_owned());
    }
    let mut decoded = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        decoded.push(if character == '\\' {
            match chars.next() {
                Some('r') => '\r',
                Some('n') => '\n',
                Some('c') => ':',
                Some('\\') => '\\',
                _ => return Err(ProtocolError::InvalidEscape),
            }
        } else {
            character
        });
    }
    Ok(decoded)
}

/// Adds an octet-counted content-length when the caller supplies a body without one.
pub(crate) fn encode(
    command: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<Vec<u8>, ProtocolError> {
    validate_command(command)?;
    if !body.is_empty() && !allows_body(command) {
        return Err(ProtocolError::MalformedFrame);
    }
    if body.len() >= MAX_FRAME_BYTES || headers.len() > MAX_HEADERS {
        return Err(ProtocolError::LimitExceeded);
    }
    let content_length = headers.iter().find(|(key, _)| *key == "content-length");
    if let Some((_, length)) = content_length
        && parse_content_length(length)? != body.len()
    {
        return Err(ProtocolError::MalformedFrame);
    }
    let add_length = !body.is_empty() && content_length.is_none();
    if headers.len() + usize::from(add_length) > MAX_HEADERS {
        return Err(ProtocolError::LimitExceeded);
    }
    let escaped = !matches!(command, "CONNECT" | "CONNECTED");
    let mut output = Vec::new();
    append_header_bytes(&mut output, command.as_bytes())?;
    append_header_bytes(&mut output, b"\n")?;
    for (key, value) in headers {
        if key.is_empty() || (!escaped && key.contains(':')) {
            return Err(ProtocolError::MalformedFrame);
        }
        encode_header(&mut output, key, escaped)?;
        append_header_bytes(&mut output, b":")?;
        encode_header(&mut output, value, escaped)?;
        append_header_bytes(&mut output, b"\n")?;
    }
    if add_length {
        append_header_bytes(&mut output, b"content-length:")?;
        append_header_bytes(&mut output, body.len().to_string().as_bytes())?;
        append_header_bytes(&mut output, b"\n")?;
    }
    append_header_bytes(&mut output, b"\n")?;
    if body.len() > MAX_FRAME_BYTES - output.len() - 1 {
        return Err(ProtocolError::LimitExceeded);
    }
    output.extend_from_slice(body);
    output.push(0);
    Ok(output)
}

fn encode_header(output: &mut Vec<u8>, value: &str, escaped: bool) -> Result<(), ProtocolError> {
    if value.contains('\0') || (!escaped && value.contains(['\r', '\n'])) {
        return Err(ProtocolError::MalformedFrame);
    }
    if !escaped {
        return append_header_bytes(output, value.as_bytes());
    }
    for byte in value.bytes() {
        match byte {
            b'\r' => append_header_bytes(output, b"\\r")?,
            b'\n' => append_header_bytes(output, b"\\n")?,
            b':' => append_header_bytes(output, b"\\c")?,
            b'\\' => append_header_bytes(output, b"\\\\")?,
            _ => append_header_bytes(output, &[byte])?,
        }
    }
    Ok(())
}

fn append_header_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), ProtocolError> {
    if bytes.len() > MAX_HEADER_BYTES - output.len() {
        return Err(ProtocolError::LimitExceeded);
    }
    output.extend_from_slice(bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_spec_message_example() {
        let frames = Decoder::new()
            .push(b"MESSAGE\nsubscription:0\nmessage-id:007\ndestination:/queue/a\ncontent-type:text/plain\n\nhello queue a\0")
            .unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].command, "MESSAGE");
        assert_eq!(frames[0].headers["destination"], "/queue/a");
        assert_eq!(frames[0].body, b"hello queue a");
    }

    #[test]
    fn handles_every_split_with_heartbeats_crlf_and_byte_counted_binary_body() {
        let wire = b"\n\r\nMESSAGE\r\ncontent-length:6\r\n\r\nh\xc3\xa9\0ok\0\nRECEIPT\nreceipt-id:42\n\n\0\r\n";
        for split in 0..=wire.len() {
            let mut decoder = Decoder::new();
            let mut frames = decoder.push(&wire[..split]).unwrap();
            frames.extend(decoder.push(&wire[split..]).unwrap());
            assert_eq!(frames.len(), 2, "split {split}");
            assert_eq!(frames[0].command, "MESSAGE");
            assert_eq!(frames[0].body, b"h\xc3\xa9\0ok");
            assert_eq!(frames[1].headers["receipt-id"], "42");
            assert!(frames[1].body.is_empty());
        }
        let mut decoder = Decoder::new();
        let mut frames = Vec::new();
        for byte in wire {
            frames.extend(decoder.push(&[*byte]).unwrap());
        }
        assert_eq!(frames.len(), 2);
    }

    #[test]
    fn decodes_escapes_without_trimming_and_preserves_first_duplicate() {
        let frames = Decoder::new().push(b"MESSAGE\nx\\ck: a\\rb\\nc\\cd\\\\e \nx\\ck:ignored\ncontent-length:1\ncontent-length:999\n\nx\0").unwrap();
        assert_eq!(frames[0].headers["x:k"], " a\rb\nc:d\\e ");
        assert_eq!(frames[0].body, b"x");
        for command in ["CONNECT", "CONNECTED"] {
            let wire = format!("{command}\npath:a\\tb:c\n\n\0");
            let frames = Decoder::new().push(wire.as_bytes()).unwrap();
            assert_eq!(frames[0].headers["path"], "a\\tb:c");
        }
        let frames = Decoder::new().push(b"STOMP\nx:a\\cb\n\n\0").unwrap();
        assert_eq!(frames[0].headers["x"], "a:b");
    }

    #[test]
    fn malformed_headers_and_commands_make_the_decoder_terminal() {
        for (wire, expected) in [
            (
                &b"MESSAGE\nx:bad\\t\n\n\0"[..],
                ProtocolError::InvalidEscape,
            ),
            (&b"MESSAGE\nx:bad\\\n\n\0"[..], ProtocolError::InvalidEscape),
            (&b"MESSAGE\nx:\xff\n\n\0"[..], ProtocolError::InvalidUtf8),
            (&b"MESSAGE\n\xff:x\n\n\0"[..], ProtocolError::InvalidUtf8),
            (&b"MESSAGE\nx\n\n\0"[..], ProtocolError::MalformedFrame),
            (&b"MESSAGE\n:x\n\n\0"[..], ProtocolError::MalformedFrame),
            (&b"MESSAGE\nx:a:b\n\n\0"[..], ProtocolError::MalformedFrame),
            (&b"MESSAGE\nx:a\rb\n\n\0"[..], ProtocolError::MalformedFrame),
            (&b"MESSAGE\nx:a\0b\n\n\0"[..], ProtocolError::MalformedFrame),
            (&b"message\n\n\0"[..], ProtocolError::MalformedFrame),
            (&b" MESSAGE\n\n\0"[..], ProtocolError::MalformedFrame),
            (&b"UNKNOWN\n\n\0"[..], ProtocolError::MalformedFrame),
            (
                &b"MESSAGE\ncontent-length:+1\n\na\0"[..],
                ProtocolError::MalformedFrame,
            ),
            (
                &b"MESSAGE\ncontent-length:-1\n\na\0"[..],
                ProtocolError::MalformedFrame,
            ),
            (
                &b"MESSAGE\ncontent-length: 1\n\na\0"[..],
                ProtocolError::MalformedFrame,
            ),
            (
                &b"MESSAGE\ncontent-length:\n\n\0"[..],
                ProtocolError::MalformedFrame,
            ),
            (
                &b"MESSAGE\ncontent-length:1\n\nab\0"[..],
                ProtocolError::MalformedFrame,
            ),
            (&b"CONNECTED\n\nbody\0"[..], ProtocolError::MalformedFrame),
        ] {
            let mut decoder = Decoder::new();
            assert_eq!(decoder.push(wire), Err(expected), "{wire:?}");
            assert_eq!(
                decoder.push(b"CONNECTED\nversion:1.2\n\n\0"),
                Err(ProtocolError::Terminal)
            );
        }
    }

    #[test]
    fn rejects_oversized_incomplete_input_and_enforces_whole_frame_budget() {
        let mut decoder = Decoder::new();
        assert!(decoder.push(b"MESSAGE\nx:").unwrap().is_empty());
        assert_eq!(
            decoder.push(&vec![b'a'; 16 * 1024]),
            Err(ProtocolError::LimitExceeded)
        );
        let many_headers = format!("MESSAGE\n{}\n\0", "x:y\n".repeat(65));
        assert_eq!(
            Decoder::new().push(many_headers.as_bytes()),
            Err(ProtocolError::LimitExceeded)
        );
        let limit_headers = format!("MESSAGE\n{}\n\0", "x:y\n".repeat(64));
        assert_eq!(
            Decoder::new().push(limit_headers.as_bytes()).unwrap().len(),
            1
        );
        assert_eq!(
            Decoder::new().push(b"MESSAGE\ncontent-length:1048576\n\n"),
            Err(ProtocolError::LimitExceeded)
        );
        assert_eq!(
            Decoder::new().push(b"MESSAGE\ncontent-length:999999999999999999999999999999\n\n"),
            Err(ProtocolError::LimitExceeded)
        );

        let mut frame = b"MESSAGE\n\n".to_vec();
        frame.resize(1024 * 1024 - 1, b'a');
        frame.push(0);
        assert_eq!(
            Decoder::new().push(&frame).unwrap()[0].body.len(),
            1024 * 1024 - 10
        );
        frame.insert(frame.len() - 1, b'a');
        assert_eq!(
            Decoder::new().push(&frame),
            Err(ProtocolError::LimitExceeded)
        );
        assert!(
            Decoder::new()
                .push(&vec![b'\n'; 2 * 1024 * 1024])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn encodes_octet_lengths_and_escapes_using_literal_wire_expectations() {
        assert_eq!(encode("SEND", &[("destination", "/queue/a"), ("a:b", "c\rd\ne\\f")], b"h\xc3\xa9\0ok").unwrap(),
            b"SEND\ndestination:/queue/a\na\\cb:c\\rd\\ne\\\\f\ncontent-length:6\n\nh\xc3\xa9\0ok\0");
        assert_eq!(
            encode(
                "CONNECT",
                &[
                    ("accept-version", "1.2"),
                    ("host", "localhost:443"),
                    ("passcode", "x\\t")
                ],
                b""
            )
            .unwrap(),
            b"CONNECT\naccept-version:1.2\nhost:localhost:443\npasscode:x\\t\n\n\0"
        );
        assert_eq!(
            encode("STOMP", &[("host", "localhost:443")], b"").unwrap(),
            b"STOMP\nhost:localhost\\c443\n\n\0"
        );
        assert_eq!(
            encode("SEND", &[("content-length", "2")], b"ok").unwrap(),
            b"SEND\ncontent-length:2\n\nok\0"
        );
    }

    #[test]
    fn encoder_rejects_injection_invalid_lengths_and_excessive_output() {
        for (command, headers, body) in [
            ("SEND\n", vec![], &b""[..]),
            ("SEND", vec![("", "x")], &b""[..]),
            ("CONNECT", vec![("host", "x\nhost:evil")], &b""[..]),
            ("CONNECTED", vec![("ho:st", "x")], &b""[..]),
            ("SEND", vec![("x", "\0")], &b""[..]),
            ("SEND", vec![("content-length", "1")], &b"ok"[..]),
            ("SEND", vec![("content-length", "-1")], &b""[..]),
            ("RECEIPT", vec![], &b"x"[..]),
        ] {
            assert_eq!(
                encode(command, &headers, body),
                Err(ProtocolError::MalformedFrame)
            );
        }
        assert_eq!(
            encode("SEND", &vec![("x", "y"); 65], b""),
            Err(ProtocolError::LimitExceeded)
        );
        assert_eq!(
            encode("SEND", &vec![("x", "y"); 64], b"x"),
            Err(ProtocolError::LimitExceeded)
        );
        assert_eq!(
            encode("SEND", &[("x", &"y".repeat(16384))], b""),
            Err(ProtocolError::LimitExceeded)
        );
        assert_eq!(
            encode("SEND", &[], &vec![0; 1024 * 1024]),
            Err(ProtocolError::LimitExceeded)
        );
    }
}
