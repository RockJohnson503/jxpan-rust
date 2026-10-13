//! Deployed RSA helper compatibility, including its nonstandard DER integer slicing.
//! Not a general-purpose ASN.1 implementation. All entropy is caller-supplied.
use super::mcloud::decode_base64_latin1;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use num_bigint::BigUint;

fn der_len(der: &[u8], offset: &mut usize) -> Result<usize, &'static str> {
    let first = *der.get(*offset).ok_or("invalid deployed DER")?;
    *offset += 1;
    if first & 128 == 0 {
        return Ok(first as usize);
    }
    let mut len = 0usize;
    for _ in 0..first & 127 {
        len = len
            .checked_mul(256)
            .and_then(|n| n.checked_add(*der.get(*offset)? as usize))
            .ok_or("invalid deployed DER")?;
        *offset += 1;
    }
    Ok(len)
}
fn integer(der: &[u8], offset: &mut usize) -> Result<Vec<u8>, &'static str> {
    *offset += 1;
    der_len(der, offset)?;
    let start = *offset;
    while der.get(*offset) == Some(&0) {
        *offset += 1;
    }
    // Original readInteger uses the LAST length octet, not decoded length.
    let length = *der
        .get(start.checked_sub(1).ok_or("invalid deployed DER")?)
        .ok_or("invalid deployed DER")? as usize;
    let bytes_len = der.len().min(length + *offset) - *offset;
    *offset = start + length;
    let begin = offset
        .checked_sub(bytes_len)
        .ok_or("invalid deployed DER")?;
    let bytes = der.get(begin..*offset).ok_or("invalid deployed DER")?;
    if bytes.is_empty() {
        return Err("invalid deployed DER");
    }
    Ok(bytes.to_vec())
}
pub fn parse_deployed_public_key(encoded: &str) -> Result<(Vec<u8>, Vec<u8>), &'static str> {
    let der: Vec<u8> = decode_base64_latin1(encoded)?
        .chars()
        .map(|c| c as u8)
        .collect();
    let mut offset = 0;
    // Six tag/length pairs, exactly as the deployed function (including NULL/BIT STRING quirks).
    for _ in 0..6 {
        offset += 1;
        der_len(&der, &mut offset)?;
    }
    let n = integer(&der, &mut offset)?;
    let e = integer(&der, &mut offset)?;
    Ok((n, e))
}
/// Exact zero-to-one padding conversion of the deployment, NOT rejection sampling.
pub fn rsa_encrypt(
    n: &[u8],
    e: &[u8],
    message: &[u8],
    padding: &[u8],
) -> Result<String, &'static str> {
    let n = n
        .iter()
        .position(|b| *b != 0)
        .map(|i| &n[i..])
        .ok_or("zero RSA modulus")?;
    // Resource guard for untrusted keys. Typical deployed keys are 1024/2048 bits.
    if n.len() > 512 || e.len() > 512 {
        return Err("RSA key too large");
    }
    let pad_len = n
        .len()
        .checked_sub(message.len() + 3)
        .filter(|n| *n >= 8)
        .ok_or("Message too long for RSA key")?;
    if padding.len() != pad_len {
        return Err("incorrect RSA entropy length");
    }
    let mut padded = vec![0, 2];
    padded.extend(padding.iter().map(|b| if *b == 0 { 1 } else { *b }));
    padded.push(0);
    padded.extend(message);
    let modulus = BigUint::from_bytes_be(n);
    let exponent = BigUint::from_bytes_be(e);
    let result = BigUint::from_bytes_be(&padded)
        .modpow(&exponent, &modulus)
        .to_bytes_be();
    // The deployment emits exactly the modulus width, including leading zeroes.
    let mut out = vec![0; n.len()];
    out[n.len() - result.len()..].copy_from_slice(&result);
    Ok(STANDARD.encode(out))
}
