use super::{Diagnostic, Error, Located, Message, Span};

#[derive(Clone, Copy)]
struct Fence {
    byte: u8,
    length: usize,
}

impl Fence {
    fn opening(line: &str) -> Option<Self> {
        let trimmed = line.trim_start_matches(' ');
        if line.len() - trimmed.len() > 3 {
            return None;
        }
        let byte = *trimmed.as_bytes().first()?;
        if !matches!(byte, b'`' | b'~') {
            return None;
        }
        let length = trimmed.bytes().take_while(|next| *next == byte).count();
        if length < 3 || (byte == b'`' && trimmed[length..].contains('`')) {
            return None;
        }
        Some(Self { byte, length })
    }

    fn closes(self, line: &str) -> bool {
        let trimmed = line.trim_start_matches(' ');
        let length = trimmed
            .bytes()
            .take_while(|next| *next == self.byte)
            .count();
        line.len() - trimmed.len() <= 3
            && length >= self.length
            && trimmed[length..].trim().is_empty()
    }
}

pub(super) fn decode(text: &str) -> (Vec<Located>, Vec<Diagnostic>) {
    let mut messages = Vec::new();
    let mut diagnostics = Vec::new();
    let mut offset = 0;
    let mut line_number = 1;
    let mut fence = None;
    let mut inline = None;
    while offset < text.len() {
        let end = line_end(text, offset);
        let line = &text[offset..end];
        let next = if end < text.len() { end + 1 } else { end };
        if let Some(open) = fence {
            if Fence::closes(open, line) {
                fence = None;
            }
        } else if inline.is_none() && Fence::opening(line).is_some() {
            fence = Fence::opening(line);
        } else if inline.is_none() && line.starts_with("LOOM_") {
            let (message_end, result) = message(text, offset, end);
            let span = Span {
                bytes: offset..message_end,
                line: line_number,
            };
            match result {
                Ok(message) => messages.push(Located { message, span }),
                Err(error) => diagnostics.push(Diagnostic { span, error }),
            }
            line_number += text[offset..message_end]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count();
            offset = message_end;
            if offset < text.len() {
                offset += 1;
                line_number += 1;
            }
            continue;
        } else {
            inline = inline_delimiter(text, offset, end, inline);
        }
        offset = next;
        line_number += 1;
    }
    (messages, diagnostics)
}

fn line_end(text: &str, start: usize) -> usize {
    text[start..]
        .find('\n')
        .map_or(text.len(), |end| start + end)
}

fn inline_delimiter(
    text: &str,
    start: usize,
    end: usize,
    mut open: Option<usize>,
) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = start;
    while index < end {
        if bytes[index] == b'\\' && open.is_none() {
            index += 2;
        } else if bytes[index] == b'`' {
            let length = bytes[index..]
                .iter()
                .take_while(|byte| **byte == b'`')
                .count();
            if open == Some(length) {
                open = None;
            } else if open.is_none() && has_closing_tick(&text[index + length..], length) {
                open = Some(length);
            }
            index += length;
        } else {
            index += 1;
        }
    }
    open
}

fn has_closing_tick(text: &str, length: usize) -> bool {
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 && (line.trim().is_empty() || Fence::opening(line).is_some()) {
            return false;
        }
        let mut bytes = line.bytes().peekable();
        while let Some(byte) = bytes.next() {
            if byte == b'`' {
                let mut run = 1;
                while bytes.peek() == Some(&b'`') {
                    bytes.next();
                    run += 1;
                }
                if run == length {
                    return true;
                }
            }
        }
    }
    false
}

fn message(text: &str, start: usize, end: usize) -> (usize, Result<Message, Error>) {
    let line = &text[start..end];
    let marker_length = line
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        .count();
    let marker = &line[..marker_length];
    let rest = &line[marker_length..];
    let (end, result) = payload(text, start + marker_length, end, marker, rest);
    if Message::recognizes(marker) {
        (end, result)
    } else {
        (
            end,
            Err(Error::UnknownMarker {
                marker: marker.to_owned(),
            }),
        )
    }
}

fn payload(
    text: &str,
    marker_end: usize,
    end: usize,
    marker: &str,
    rest: &str,
) -> (usize, Result<Message, Error>) {
    if rest.trim().is_empty() {
        return (end, Message::parse(marker, None));
    }
    let has_colon = rest.starts_with(':');
    let payload_start = marker_end + usize::from(has_colon);
    let payload = text[payload_start..].trim_start_matches([' ', '\t', '\r', '\n']);
    if !payload.starts_with(['{', '[', '"']) {
        return (
            end,
            if has_colon {
                Message::parse(marker, Some(&text[payload_start..end]))
            } else {
                Err(Error::MarkerSuffix)
            },
        );
    }
    let json_start = text.len() - payload.len();
    let Some(json_end) = json_end(text, json_start) else {
        return (text.len(), Err(Error::UnterminatedObject));
    };
    let closing_line_end = line_end(text, json_end);
    if !has_colon {
        return (closing_line_end, Err(Error::MarkerSuffix));
    }
    if !text[json_end..closing_line_end].trim().is_empty() {
        return (closing_line_end, Err(Error::ObjectSuffix));
    }
    (
        closing_line_end,
        Message::parse(marker, Some(&text[json_start..json_end])),
    )
}

fn json_end(text: &str, start: usize) -> Option<usize> {
    let mut stack = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    for (relative, byte) in text.as_bytes()[start..].iter().copied().enumerate() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
                if stack.is_empty() {
                    return Some(start + relative + 1);
                }
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => stack.push(byte),
                b'}' | b']' => {
                    let expected = if byte == b'}' { b'{' } else { b'[' };
                    if stack.pop() != Some(expected) {
                        return None;
                    }
                    if stack.is_empty() {
                        return Some(start + relative + 1);
                    }
                }
                _ => {}
            }
        }
    }
    None
}
