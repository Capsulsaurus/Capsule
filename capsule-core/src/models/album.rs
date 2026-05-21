pub enum AlbumAccess {
    Owner,
    Write,
    Read,
}

impl AlbumAccess {
    /// Returns whether user has write access to album
    pub fn is_write(&self) -> bool {
        matches!(self, AlbumAccess::Owner | AlbumAccess::Write)
    }
}
