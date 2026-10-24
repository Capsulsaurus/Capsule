use uuid::Uuid;

/// Short form of an id: its last eight hex digits, since a UUIDv7 leads with its creation time
/// and ids minted together share their first eight.
pub(crate) fn short_id(id: Uuid) -> String {
    let simple = id.simple().to_string();
    simple[simple.len() - 8..].to_owned()
}

#[cfg(test)]
mod tests;
