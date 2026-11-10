// Strict bounded RTSP/HTTP control-channel framing; PlayPort wire behavior, GPL-3.0-only.
use crate::{Error, Result};
use std::collections::BTreeMap;
const MAX_HEADER: usize = 16384;
const MAX_BODY: usize = 262144;
#[derive(Debug)]
pub struct Message {
    pub method: String,
    pub path: String,
    pub protocol: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
#[derive(Default)]
pub struct Decoder {
    buffer: Vec<u8>,
    rejection: Option<&'static str>,
}
impl Decoder {
    pub fn feed(&mut self, b: &[u8]) -> Result<()> {
        if self.buffer.len() + b.len() > MAX_HEADER + MAX_BODY {
            return Err(Error::Limit);
        }
        self.buffer.extend(b);
        Ok(())
    }
    pub fn next_message(&mut self) -> Result<Option<Message>> {
        self.rejection = None;
        self.parse_next().map_err(|(error, reason)| {
            self.rejection = Some(reason);
            error
        })
    }
    /// Fixed diagnostic category only; never includes wire data or header names/values.
    pub fn rejection(&self) -> Option<&'static str> {
        self.rejection
    }
    fn parse_next(&mut self) -> std::result::Result<Option<Message>, (Error, &'static str)> {
        let Some(at) = self.buffer.windows(4).position(|w| w == b"\r\n\r\n") else {
            if self.buffer.len() > MAX_HEADER {
                return Err((Error::Limit, "HEADER_LIMIT"));
            }
            return Ok(None);
        };
        if at > MAX_HEADER {
            return Err((Error::Limit, "HEADER_LIMIT"));
        }
        let text = std::str::from_utf8(&self.buffer[..at])
            .map_err(|_| (Error::Malformed, "HEADER_UTF8"))?;
        let mut lines = text.split("\r\n");
        let first = lines.next().ok_or((Error::Malformed, "REQUEST_LINE"))?;
        if !first.is_ascii() || first.bytes().any(|b| b < 0x20 || b == 0x7f) {
            return Err((Error::Malformed, "REQUEST_LINE"));
        }
        let words: Vec<&str> = first.split(' ').collect();
        if words.len() != 3 {
            return Err((Error::Malformed, "REQUEST_LINE"));
        }
        let mut headers = BTreeMap::new();
        for line in lines {
            let (key, value) = line
                .split_once(':')
                .ok_or((Error::Malformed, "HEADER_LINE"))?;
            let key = key.to_ascii_lowercase();
            if key.is_empty()
                || !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
                || headers.len() >= 64
            {
                return Err((Error::Malformed, "HEADER_NAME_OR_COUNT"));
            }
            // RTSP uses UTF-8 (RFC 2326 section 4). Header values can contain a phone's name,
            // while framing controls remain invalid even inside otherwise valid UTF-8.
            if value.bytes().any(|b| b == 0x7f || (b < 0x20 && b != b'\t')) {
                return Err((Error::Malformed, "HEADER_VALUE_CONTROL"));
            }
            if headers
                .insert(key, value.trim_matches([' ', '\t']).to_owned())
                .is_some()
            {
                return Err((Error::Malformed, "DUPLICATE_HEADER"));
            }
        }
        if headers.contains_key("transfer-encoding") {
            return Err((Error::Malformed, "TRANSFER_ENCODING"));
        }
        let len = match headers.get("content-length") {
            Some(s) => {
                if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
                    return Err((Error::Malformed, "CONTENT_LENGTH"));
                }
                s.parse::<usize>()
                    .map_err(|_| (Error::Limit, "CONTENT_LENGTH_LIMIT"))?
            }
            None => 0,
        };
        if len > MAX_BODY {
            return Err((Error::Limit, "BODY_LIMIT"));
        }
        let end = at + 4 + len;
        if self.buffer.len() < end {
            return Ok(None);
        }
        let message = Message {
            method: words[0].to_owned(),
            path: words[1].to_owned(),
            protocol: words[2].to_owned(),
            headers,
            body: self.buffer[at + 4..end].to_vec(),
        };
        if !message.method.starts_with("RTSP/")
            && !message.method.starts_with("HTTP/")
            && message.protocol != "RTSP/1.0"
            && message.protocol != "HTTP/1.1"
        {
            return Err((Error::Malformed, "PROTOCOL"));
        }
        self.buffer.drain(..end);
        Ok(Some(message))
    }
    pub fn eof(&mut self) -> Result<()> {
        let empty = self.buffer.is_empty();
        self.buffer.clear();
        if empty {
            Ok(())
        } else {
            Err(Error::Truncated)
        }
    }
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}
pub fn response(
    request: &Message,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<Vec<u8>> {
    if body.len() > MAX_BODY || content_type.contains(['\r', '\n']) {
        return Err(Error::Limit);
    }
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        461 => "Unsupported Transport",
        _ => "Error",
    };
    let protocol = if request.protocol == "HTTP/1.1" {
        "HTTP/1.1"
    } else {
        "RTSP/1.0"
    };
    let mut head = format!(
        "{protocol} {status} {reason}\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\n",
        body.len()
    );
    if let Some(cseq) = request.headers.get("cseq") {
        if cseq.len() > 16 || !cseq.bytes().all(|c| c.is_ascii_digit()) {
            return Err(Error::Malformed);
        }
        head.push_str(&format!("CSeq: {cseq}\r\n"));
    }
    head.push_str("\r\n");
    let mut out = head.into_bytes();
    out.extend(body);
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf8_header_value_preserves_fragmented_pair_verify_body() {
        let request = "POST /pair-verify RTSP/1.0\r\nCSeq: 7\r\nX-Apple-Client-Name: 测试手机\r\nContent-Length: 3\r\n\r\nabc".as_bytes();
        for split in 0..request.len() {
            let mut decoder = Decoder::default();
            decoder.feed(&request[..split]).unwrap();
            assert!(decoder.next_message().unwrap().is_none());
            decoder.feed(&request[split..]).unwrap();
            let message = decoder.next_message().unwrap().unwrap();
            assert_eq!(message.headers["x-apple-client-name"], "测试手机");
            assert_eq!(message.body, b"abc");
            decoder.eof().unwrap();
        }
    }
    #[test]
    fn utf8_values_do_not_allow_ambiguous_or_control_headers() {
        for request in [
            "POST /测试 RTSP/1.0\r\n\r\n",
            "POST / RTSP/1.0\r\n手机: value\r\n\r\n",
            "POST / RTSP/1.0\r\nX-Name: 测试\0手机\r\n\r\n",
            "POST / RTSP/1.0\r\nX-Name: 测试\n手机\r\n\r\n",
            "POST / RTSP/1.0\r\nX-Name: 测试\r手机\r\n\r\n",
            "POST / RTSP/1.0\r\nX-Name: 测试\u{7f}手机\r\n\r\n",
            "POST / RTSP/1.0\r\nX-Name: 测试\r\nx-name: 手机\r\n\r\n",
            "POST / RTSP/1.0\r\nContent-Length: \u{a0}3\u{a0}\r\n\r\nabc",
        ] {
            let mut decoder = Decoder::default();
            decoder.feed(request.as_bytes()).unwrap();
            assert!(decoder.next_message().is_err());
        }
        let mut decoder = Decoder::default();
        decoder
            .feed("POST / RTSP/1.0\r\nCSeq: \u{a0}7\u{a0}\r\n\r\n".as_bytes())
            .unwrap();
        let message = decoder.next_message().unwrap().unwrap();
        assert!(response(&message, 200, "application/octet-stream", b"").is_err());
    }
    #[test]
    fn fragments_and_pipeline() {
        let req = b"POST /pair-setup RTSP/1.0\r\nCSeq: 7\r\nContent-Length: 3\r\n\r\nabc";
        for split in 0..req.len() {
            let mut d = Decoder::default();
            d.feed(&req[..split]).unwrap();
            assert!(d.next_message().unwrap().is_none());
            d.feed(&req[split..]).unwrap();
            let m = d.next_message().unwrap().unwrap();
            assert_eq!(m.body, b"abc");
            assert!(String::from_utf8(
                response(&m, 200, "application/octet-stream", b"ok").unwrap()
            )
            .unwrap()
            .contains("CSeq: 7"));
            d.eof().unwrap();
        }
    }
    #[test]
    fn malformed_lengths_and_duplicates() {
        for h in [
            "Content-Length: -1",
            "Content-Length: 3\r\nContent-Length: 4",
            "Content-Length: 99999999999",
            "Transfer-Encoding: chunked",
        ] {
            let mut d = Decoder::default();
            d.feed(format!("POST / RTSP/1.0\r\n{h}\r\n\r\n").as_bytes())
                .unwrap();
            assert!(d.next_message().is_err())
        }
    }
    #[test]
    fn disconnect_with_incomplete_body() {
        let mut d = Decoder::default();
        d.feed(b"POST / RTSP/1.0\r\nContent-Length: 3\r\n\r\na")
            .unwrap();
        assert!(d.next_message().unwrap().is_none());
        assert_eq!(d.eof(), Err(Error::Truncated));
    }
}
