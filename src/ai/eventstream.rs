//! AWS event stream binary framing (`application/vnd.amazon.eventstream`),
//! used by Bedrock ConverseStream and InvokeModelWithResponseStream.
//!
//! Message layout (all integers big-endian):
//!
//! ```text
//! [total length u32][headers length u32][prelude CRC32 u32]
//! [headers ...][payload ...][message CRC32 u32]
//! ```
//!
//! The prelude CRC covers the first 8 bytes, the message CRC everything
//! before it. A header is `[name len u8][name][type u8][value]`; this
//! module writes string values (type 7: `[len u16][utf-8 bytes]`).

/// CRC-32 (IEEE 802.3, the zlib/PNG polynomial).
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        t
    });
    let mut crc = 0xFFFF_FFFFu32;
    for b in data {
        crc = table[((crc ^ u32::from(*b)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// Encode one message with string headers.
pub fn encode(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    let mut hbuf = Vec::new();
    for (name, value) in headers {
        let name = &name.as_bytes()[..name.len().min(255)];
        let value = &value.as_bytes()[..value.len().min(u16::MAX as usize)];
        hbuf.push(name.len() as u8);
        hbuf.extend_from_slice(name);
        hbuf.push(7);
        hbuf.extend_from_slice(&(value.len() as u16).to_be_bytes());
        hbuf.extend_from_slice(value);
    }
    let total = 12 + hbuf.len() + payload.len() + 4;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(total as u32).to_be_bytes());
    out.extend_from_slice(&(hbuf.len() as u32).to_be_bytes());
    let prelude_crc = crc32(&out[..8]);
    out.extend_from_slice(&prelude_crc.to_be_bytes());
    out.extend_from_slice(&hbuf);
    out.extend_from_slice(payload);
    let msg_crc = crc32(&out);
    out.extend_from_slice(&msg_crc.to_be_bytes());
    out
}

/// An `event` message (`:event-type`, `:content-type`, `:message-type`).
pub fn event(event_type: &str, payload: &serde_json::Value) -> Vec<u8> {
    encode(
        &[
            (":event-type", event_type),
            (":content-type", "application/json"),
            (":message-type", "event"),
        ],
        payload.to_string().as_bytes(),
    )
}

/// An `exception` message (mid-stream error).
pub fn exception(exception_type: &str, message: &str) -> Vec<u8> {
    encode(
        &[
            (":exception-type", exception_type),
            (":content-type", "application/json"),
            (":message-type", "exception"),
        ],
        serde_json::json!({ "message": message })
            .to_string()
            .as_bytes(),
    )
}

/// A decoded message (string headers only).
#[derive(Debug, PartialEq, Eq)]
pub struct Message {
    pub headers: Vec<(String, String)>,
    pub payload: Vec<u8>,
}

impl Message {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn be32(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| "truncated".to_string())
}

/// Decode a buffer of concatenated messages, verifying both CRCs.
pub fn decode_all(mut buf: &[u8]) -> Result<Vec<Message>, String> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        let total = be32(buf, 0)? as usize;
        let hlen = be32(buf, 4)? as usize;
        let pcrc = be32(buf, 8)?;
        if total < 16 || total > buf.len() || 12 + hlen + 4 > total {
            return Err("bad lengths".into());
        }
        if crc32(&buf[..8]) != pcrc {
            return Err("prelude crc mismatch".into());
        }
        let mcrc = be32(buf, total - 4)?;
        if crc32(&buf[..total - 4]) != mcrc {
            return Err("message crc mismatch".into());
        }
        let mut headers = Vec::new();
        let hb = &buf[12..12 + hlen];
        let mut i = 0;
        while i < hb.len() {
            let nl = hb[i] as usize;
            let name = String::from_utf8_lossy(hb.get(i + 1..i + 1 + nl).ok_or("bad header")?)
                .into_owned();
            i += 1 + nl;
            let ty = *hb.get(i).ok_or("bad header type")?;
            if ty != 7 {
                return Err(format!("unsupported header type {ty}"));
            }
            let vl = hb
                .get(i + 1..i + 3)
                .map(|s| u16::from_be_bytes([s[0], s[1]]) as usize)
                .ok_or("bad header value")?;
            let value =
                String::from_utf8_lossy(hb.get(i + 3..i + 3 + vl).ok_or("bad value")?).into_owned();
            i += 3 + vl;
            headers.push((name, value));
        }
        out.push(Message {
            headers,
            payload: buf[12 + hlen..total - 4].to_vec(),
        });
        buf = &buf[total..];
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_known_values() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    #[test]
    fn round_trip_and_crc_checks() {
        let a = event("messageStart", &serde_json::json!({"role": "assistant"}));
        let b = event(
            "messageStop",
            &serde_json::json!({"stopReason": "end_turn"}),
        );
        let mut buf = a.clone();
        buf.extend_from_slice(&b);
        let msgs = decode_all(&buf).expect("decode");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].header(":event-type"), Some("messageStart"));
        assert_eq!(msgs[0].header(":message-type"), Some("event"));
        assert_eq!(msgs[1].payload, br#"{"stopReason":"end_turn"}"#.to_vec());
        // Total length field matches.
        assert_eq!(
            u32::from_be_bytes([a[0], a[1], a[2], a[3]]) as usize,
            a.len()
        );
        // Corruption is detected.
        let mut bad = a.clone();
        let last = bad.len() - 6;
        bad[last] ^= 0xFF;
        assert!(decode_all(&bad).is_err());
        let mut bad = a;
        bad[1] ^= 0x01;
        assert!(decode_all(&bad).is_err());
    }

    #[test]
    fn empty_payload_message() {
        let m = encode(&[], b"");
        assert_eq!(m.len(), 16);
        assert_eq!(decode_all(&m).expect("decode")[0].payload.len(), 0);
    }
}
