//! Provider-compatible JSON decoding without rounding long integer tokens.
use serde_json::Value;

// Same lexical scanner as jxpanParseJsonSafe: quote integer tokens with >=16
// digits before serde_json sees them, including negative IDs and sizes.
pub(crate) fn parse_json_safe(text: &str) -> serde_json::Result<Value> {
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            output.push(bytes[i]);
            i += 1;
            while i < bytes.len() {
                let byte = bytes[i];
                output.push(byte);
                i += 1;
                if byte == b'\\' && i < bytes.len() {
                    output.push(bytes[i]);
                    i += 1;
                } else if byte == b'"' {
                    break;
                }
            }
            continue;
        }
        let byte = bytes[i];
        if (byte == b'-' || byte.is_ascii_digit())
            && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || b"_\"".contains(&bytes[i - 1])))
        {
            let mut j = i + usize::from(byte == b'-');
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j - i - usize::from(byte == b'-') >= 16 {
                output.push(b'"');
                output.extend_from_slice(&bytes[i..j]);
                output.push(b'"');
            } else {
                output.extend_from_slice(&bytes[i..j]);
            }
            i = j;
            continue;
        }
        output.push(byte);
        i += 1;
    }
    serde_json::from_slice(&output)
}
