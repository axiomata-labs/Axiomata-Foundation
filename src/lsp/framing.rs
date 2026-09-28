//! The Language Server Protocol's base framing: each message is a header block
//! (`Content-Length: <bytes>`, optionally `Content-Type`), an empty line, and
//! exactly that many bytes of UTF-8 JSON.

use std::io::{self, BufRead};

/// Largest message accepted from a server. A workspace-wide symbol list or a
/// large file's semantic tokens stay far below this; the cap only keeps a
/// misbehaving server from growing a buffer without end.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// Frames one JSON message for a server's stdin.
pub fn encode(message: &str) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", message.len()).into_bytes();
    out.extend_from_slice(message.as_bytes());
    out
}

/// Reads the next message from a server's stdout; `Ok(None)` at the end of the
/// stream.
///
/// Errors:
///     `InvalidData` for a header block without a usable `Content-Length`, a
///     length over [`MAX_MESSAGE_BYTES`], or a body that is not UTF-8.
pub fn read_message(input: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut length: Option<usize> = None;
    let mut line = String::new();
    let mut any_header = false;
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return if any_header {
                Err(invalid("the stream ended inside a header block"))
            } else {
                Ok(None)
            };
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            if any_header {
                break;
            }
            // Stray blank lines between messages are tolerated.
            continue;
        }
        any_header = true;
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            let parsed = value
                .trim()
                .parse::<usize>()
                .map_err(|_| invalid("a bad Content-Length"))?;
            length = Some(parsed);
        }
    }
    let length = length.ok_or_else(|| invalid("a header block without Content-Length"))?;
    if length > MAX_MESSAGE_BYTES {
        return Err(invalid("a message larger than the limit"));
    }
    let mut body = vec![0; length];
    input.read_exact(&mut body)?;
    String::from_utf8(body)
        .map(Some)
        .map_err(|_| invalid("a message that is not UTF-8"))
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("language server sent {what}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn a_message_round_trips_and_counts_bytes_not_characters() {
        let message = r#"{"jsonrpc":"2.0","method":"x","params":{"text":"Grüße"}}"#;
        let framed = encode(message);
        assert!(
            framed.starts_with(format!("Content-Length: {}\r\n\r\n", message.len()).as_bytes())
        );
        let mut input = Cursor::new(framed);
        assert_eq!(read_message(&mut input).unwrap().as_deref(), Some(message));
        assert_eq!(read_message(&mut input).unwrap(), None);
    }

    #[test]
    fn messages_follow_each_other_with_extra_headers() {
        let mut stream = encode("{\"a\":1}");
        stream.extend_from_slice(b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n");
        stream.extend_from_slice(b"content-length: 7\r\n\r\n{\"b\":2}");
        let mut input = Cursor::new(stream);
        assert_eq!(
            read_message(&mut input).unwrap().as_deref(),
            Some("{\"a\":1}")
        );
        assert_eq!(
            read_message(&mut input).unwrap().as_deref(),
            Some("{\"b\":2}")
        );
    }

    #[test]
    fn broken_headers_and_oversized_messages_are_refused() {
        for bad in [
            b"Content-Type: x\r\n\r\n{}".to_vec(),
            b"Content-Length: nope\r\n\r\n{}".to_vec(),
            format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE_BYTES + 1).into_bytes(),
            b"Content-Length: 5\r\n".to_vec(),
        ] {
            let err = read_message(&mut Cursor::new(bad)).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        }
        let err =
            read_message(&mut Cursor::new(b"Content-Length: 9\r\n\r\n{}".to_vec())).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof, "a body cut short");
    }
}
