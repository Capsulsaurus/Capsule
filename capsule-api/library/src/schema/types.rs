use async_graphql::*;

#[derive(SimpleObject)]
pub struct Tag {
    pub id: String,
    pub name: String,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq)]
pub enum SortDirection {
    /// Ascending
    Asc,
    /// Descending
    Desc,
}
