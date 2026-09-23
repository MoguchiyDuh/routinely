use axum::http::HeaderMap;
use axum::http::header;

pub fn get(headers: &HeaderMap, name: &str) -> Option<String> {
    for value in headers.get_all(header::COOKIE) {
        let Ok(raw) = value.to_str() else {
            continue;
        };
        for pair in raw.split(';') {
            let Some((key, val)) = pair.trim().split_once('=') else {
                continue;
            };
            if key.trim() == name && !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    return None;
}

pub fn encode(text: &str, max_len: usize) -> String {
    let mut out = String::new();
    let mut buf = [0u8; 4];
    for ch in text.chars() {
        let mut piece = String::new();
        for byte in ch.encode_utf8(&mut buf).bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                piece.push(char::from(byte));
            } else {
                piece.push_str(&format!("%{byte:02X}"));
            }
        }
        if out.len() + piece.len() > max_len {
            break;
        }
        out.push_str(&piece);
    }
    return out;
}

pub fn decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%' {
            let hex = raw.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(byte);
            index += 1;
        }
    }
    return String::from_utf8(out).ok();
}
