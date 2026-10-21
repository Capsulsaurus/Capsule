//! User labels (SSoT: [Organization — Labels]).
//!
//! A label is a Unicode string, NFC-normalized and trimmed, 1–256 bytes, case-preserving. It
//! carries no hierarchy and no type. The same rule holds at every level a label attaches to —
//! asset (`tags_user`), image region, stack and album — so one normalizer owns it.
//!
//! [Organization — Labels]: https://docs/design/organization/#labels

use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

/// The largest label, in UTF-8 bytes after normalization.
pub const MAX_LABEL_BYTES: usize = 256;

/// Why a label was refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LabelError {
    /// Nothing is left once the label is trimmed.
    #[error("label is empty")]
    Empty,
    /// The normalized label is longer than [`MAX_LABEL_BYTES`].
    #[error("label is {0} bytes; the limit is {MAX_LABEL_BYTES}")]
    TooLong(usize),
}

/// Normalize `raw` into a storable label: NFC, then trimmed of surrounding whitespace, then
/// checked against the 1–[`MAX_LABEL_BYTES`] byte bound. Case is preserved.
///
/// # Errors
///
/// [`LabelError::Empty`] when only whitespace remains, [`LabelError::TooLong`] past the bound.
pub fn normalize_label(raw: &str) -> Result<String, LabelError> {
    let nfc: String = raw.nfc().collect();
    let trimmed = nfc.trim();
    if trimmed.is_empty() {
        return Err(LabelError::Empty);
    }
    if trimmed.len() > MAX_LABEL_BYTES {
        return Err(LabelError::TooLong(trimmed.len()));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests;
