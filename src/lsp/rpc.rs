//! The language server wire format: JSON-RPC messages, each after a `Content-Length` header.

use std::io::{self, BufRead, Write};

use serde_json::Value;

pub fn write_message(out: &mut impl Write, message: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(out, "Content-Length: {}\r\n\r\n", body.len())?;
    out.write_all(&body)?;
    out.flush()
}

/// The next message, or `None` at the end of the stream. Headers other than `Content-Length` are skipped.
pub fn read_message(input: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let length = length.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "a message without Content-Length"))?;
    let mut body = vec![0; length];
    input.read_exact(&mut body)?;
    serde_json::from_slice(&body).map(Some).map_err(io::Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn messages_round_trip_and_extra_headers_are_skipped() {
        let mut wire = Vec::new();
        write_message(&mut wire, &json!({"jsonrpc": "2.0", "id": 1, "result": "é"})).unwrap();
        wire.extend_from_slice(b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: 2\r\n\r\n{}");
        let mut input = io::Cursor::new(wire);
        assert_eq!(read_message(&mut input).unwrap(), Some(json!({"jsonrpc": "2.0", "id": 1, "result": "é"})));
        assert_eq!(read_message(&mut input).unwrap(), Some(json!({})));
        assert_eq!(read_message(&mut input).unwrap(), None);
    }

    #[test]
    fn a_message_without_length_is_an_error() {
        let mut input = io::Cursor::new(b"X-Other: 1\r\n\r\n{}".to_vec());
        assert!(read_message(&mut input).is_err());
    }
}
