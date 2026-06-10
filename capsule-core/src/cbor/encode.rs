//! The canonical CBOR byte encoder over [`ciborium::value::Value`].
//!
//! Implements RFC 8949 §4.2 *Core Deterministic Encoding*:
//! - definite-length items only,
//! - shortest-form (preferred) integer and float encodings,
//! - map entries sorted by the **bytewise lexicographic order of their encoded keys**.
//!
//! `ciborium` itself preserves map wire order and does not offer a deterministic mode,
//! so this encoder owns the framing; `ciborium` is used only to model values.

use ciborium::value::{Integer, Value};

const MT_UINT: u8 = 0;
const MT_NINT: u8 = 1;
const MT_BYTES: u8 = 2;
const MT_TEXT: u8 = 3;
const MT_ARRAY: u8 = 4;
const MT_MAP: u8 = 5;
const MT_TAG: u8 = 6;

/// Emit a major-type head with its argument in the shortest of the 1/2/3/5/9-byte forms.
fn encode_head(major: u8, arg: u64, out: &mut Vec<u8>) {
    let mt = major << 5;
    if arg < 24 {
        out.push(mt | arg as u8);
    } else if arg < 0x100 {
        out.push(mt | 24);
        out.push(arg as u8);
    } else if arg < 0x1_0000 {
        out.push(mt | 25);
        out.extend_from_slice(&(arg as u16).to_be_bytes());
    } else if arg < 0x1_0000_0000 {
        out.push(mt | 26);
        out.extend_from_slice(&(arg as u32).to_be_bytes());
    } else {
        out.push(mt | 27);
        out.extend_from_slice(&arg.to_be_bytes());
    }
}

fn encode_integer(i: Integer, out: &mut Vec<u8>) {
    // CBOR integers span -2^64 ..= 2^64-1, all of which fit in i128.
    let n: i128 = i.into();
    if n >= 0 {
        encode_head(MT_UINT, n as u64, out);
    } else {
        // Negative integers encode the argument as (-1 - n), itself in 0 ..= 2^64-1.
        encode_head(MT_NINT, (-1 - n) as u64, out);
    }
}

/// RFC 8949 §4.2.1 preferred float serialization: the shortest of f16/f32/f64 that
/// round-trips the value exactly. All NaNs collapse to the canonical quiet NaN `0xf97e00`.
fn encode_float(f: f64, out: &mut Vec<u8>) {
    if f.is_nan() {
        out.extend_from_slice(&[0xf9, 0x7e, 0x00]);
        return;
    }
    let h = half::f16::from_f64(f);
    if h.to_f64().to_bits() == f.to_bits() {
        out.push(0xf9);
        out.extend_from_slice(&h.to_be_bytes());
        return;
    }
    let s = f as f32;
    if (s as f64).to_bits() == f.to_bits() {
        out.push(0xfa);
        out.extend_from_slice(&s.to_be_bytes());
        return;
    }
    out.push(0xfb);
    out.extend_from_slice(&f.to_be_bytes());
}

fn encode_map(entries: &[(Value, Value)], out: &mut Vec<u8>) {
    // Encode each (key, value) to its own bytes, then sort entries by the encoded key
    // bytes (bytewise lexicographic), per RFC 8949 §4.2.1.
    let mut encoded: Vec<(Vec<u8>, Vec<u8>)> = entries
        .iter()
        .map(|(k, v)| {
            let mut kb = Vec::new();
            encode_value(k, &mut kb);
            let mut vb = Vec::new();
            encode_value(v, &mut vb);
            (kb, vb)
        })
        .collect();
    encoded.sort_by(|a, b| a.0.cmp(&b.0));

    encode_head(MT_MAP, encoded.len() as u64, out);
    for (kb, vb) in encoded {
        out.extend_from_slice(&kb);
        out.extend_from_slice(&vb);
    }
}

/// Recursively encode a value in canonical form.
pub(crate) fn encode_value(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Integer(i) => encode_integer(*i, out),
        Value::Bytes(b) => {
            encode_head(MT_BYTES, b.len() as u64, out);
            out.extend_from_slice(b);
        }
        Value::Text(s) => {
            encode_head(MT_TEXT, s.len() as u64, out);
            out.extend_from_slice(s.as_bytes());
        }
        Value::Array(a) => {
            encode_head(MT_ARRAY, a.len() as u64, out);
            for e in a {
                encode_value(e, out);
            }
        }
        Value::Map(m) => encode_map(m, out),
        Value::Tag(t, inner) => {
            encode_head(MT_TAG, *t, out);
            encode_value(inner, out);
        }
        Value::Bool(false) => out.push(0xf4),
        Value::Bool(true) => out.push(0xf5),
        Value::Float(f) => encode_float(*f, out),
        // `Value::Null` and any future variant (`ciborium::Value` is `#[non_exhaustive]`)
        // have no special canonical form here, so encode as CBOR null rather than panic.
        _ => out.push(0xf6),
    }
}
