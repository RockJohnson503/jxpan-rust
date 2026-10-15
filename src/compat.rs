//! Pure JavaScript compatibility helpers for JSON values and strings.
use serde_json::Value;

/// JavaScript truthiness in the JSON domain: empty arrays and objects are truthy.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// Borrow the first truthy property, preserving absence rather than choosing a default.
pub fn first_truthy_ref<'a>(row: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .find_map(|key| row.get(*key).filter(|v| truthy(v)))
}

/// The original's || chains preserve the selected name/size's JSON type.
pub fn first_truthy(row: &Value, keys: &[&str], fallback: Value) -> Value {
    first_truthy_ref(row, keys).cloned().unwrap_or(fallback)
}

// Unlike form decoding: '+' is unchanged, malformed escapes/UTF-8 reject the
// whole operation, and percent-encoded reserved characters are decoded too.
pub(crate) fn decode_uri_component(input: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(input.len());
    let source = input.as_bytes();
    let mut i = 0;
    while i < source.len() {
        if source[i] == b'%' {
            let hi = (*source.get(i + 1)? as char).to_digit(16)?;
            let lo = (*source.get(i + 2)? as char).to_digit(16)?;
            bytes.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            bytes.push(source[i]);
            i += 1;
        }
    }
    String::from_utf8(bytes).ok()
}

/// JavaScript encodeURIComponent: preserve its ASCII punctuation, encode UTF-8 bytes.
pub fn encode_uri_component(input: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            write!(&mut out, "%{byte:02X}").expect("String write");
        }
    }
    out
}

/// JavaScript parseInt with inferred decimal/hex radix; None represents NaN.
pub fn parse_int(input: &str) -> Option<f64> {
    let mut s = input.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    let sign = if let Some(rest) = s.strip_prefix('-') {
        s = rest;
        -1.0
    } else {
        if let Some(rest) = s.strip_prefix('+') {
            s = rest;
        }
        1.0
    };
    let radix = if s.starts_with("0x") || s.starts_with("0X") {
        s = &s[2..];
        16
    } else {
        10
    };
    let digits = s
        .chars()
        .take_while(|c| c.is_ascii() && c.is_digit(radix))
        .count();
    if digits == 0 {
        return None;
    }
    let value = if radix == 10 {
        s[..digits].parse::<f64>().unwrap_or(f64::INFINITY)
    } else {
        s[..digits].chars().fold(0.0, |n, c| {
            n * 16.0 + c.to_digit(16).expect("hex digit") as f64
        })
    };
    Some(sign * value)
}

/// JSON-domain JavaScript String coercion (objects/arrays are not JSON.stringify).
pub fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => {
            let number = n.as_f64().unwrap_or(0.0);
            if number == 0.0 {
                "0".into()
            } else if number.abs() >= 1e21 || number.abs() < 1e-6 {
                scientific(number)
            } else {
                number.to_string()
            }
        }
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    js_string(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

fn scientific(n: f64) -> String {
    let s = format!("{n:e}");
    let (mantissa, exponent) = s.split_once('e').expect("float exponent");
    let exponent: i32 = exponent.parse().expect("float exponent integer");
    format!(
        "{mantissa}e{}{exponent}",
        if exponent >= 0 { "+" } else { "" }
    )
}

// toFixed rounds exact halfway values up, unlike Rust's ties-to-even formatter.
fn fixed_two(n: f64) -> String {
    if n == 0.0 {
        return "0.00".into();
    }
    let bits = n.abs().to_bits();
    let mantissa = ((bits & ((1u64 << 52) - 1)) | (1u64 << 52)) as u128 * 100;
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1023 - 52;
    let cents = if exponent >= 0 {
        mantissa << exponent
    } else if -exponent >= 128 {
        0
    } else {
        let shift = -exponent as u32;
        (mantissa + (1u128 << (shift - 1))) >> shift
    };
    format!(
        "{}{}.{:02}",
        if n < 0.0 { "-" } else { "" },
        cents / 100,
        cents % 100
    )
}

/// readable.js formatFileSize: parseInt (including numeric prefixes and hex), then toFixed(2).
/// Invalid input is deliberately "NaN B", not "0 B" or "未知大小".
pub fn format_file_size(size: &Value) -> String {
    let Some(mut size) = parse_int(&js_string(size)) else {
        return "NaN B".into();
    };
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut unit = 0;
    while size >= 1024.0 && unit < 4 {
        size /= 1024.0;
        unit += 1;
    }
    // JS toFixed returns exponential notation for magnitudes >= 1e21.
    let number = if size.is_infinite() {
        if size.is_sign_negative() {
            "-Infinity".into()
        } else {
            "Infinity".into()
        }
    } else if size.abs() >= 1e21 {
        scientific(size)
    } else {
        fixed_two(size)
    };
    format!("{number} {}", units[unit])
}
