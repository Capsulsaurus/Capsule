//! Smart-album label search (SSoT: [Organization — Labels] and
//! [Organization — Smart-Album Definition Schema]).
//!
//! A smart-album `contains` term over `tags_user` matches a label at **any level attached to
//! the asset**: the asset's own `tags_user`, the labels of each of its live regions, its stack's
//! [`StackDocV1`] and its container album's [`AlbumDocV1`]. A deleted region (a stamped `None`
//! in its register) contributes nothing, and neither does a stack record the asset is not a
//! current member of.
//!
//! This module evaluates that one term. The rest of the predicate grammar (`All`/`Any`/`Not`,
//! the other fields and operators) is not built yet; when it is, its `tags_user` `contains`
//! term delegates here so every level stays covered by one function.
//!
//! Evaluation is a pure function of its inputs: the operand is compared byte-for-byte against
//! stored labels, which are already NFC-normalized and trimmed by
//! [`normalize_label`](super::normalize_label), so matching is case-sensitive like the labels
//! themselves.
//!
//! [Organization — Labels]: https://docs/design/organization/#labels
//! [Organization — Smart-Album Definition Schema]: https://docs/design/organization/#smart-album-definition-schema

use super::album_doc::AlbumDocV1;
use super::region::live_region;
use super::sidecar_v1::SidecarV1;
use super::stack_doc::StackDocV1;
use crate::metadata::crdt::OrSet;

/// The level a matched label sits at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelLevel {
    /// The asset's own `tags_user`.
    Asset,
    /// A live region of the asset.
    Region,
    /// The stack the asset is currently a member of.
    Stack,
    /// The asset's container album.
    Album,
}

/// Everything a `tags_user` term can see for one asset: its sidecar plus the records of the
/// stack and container album it belongs to, where the caller holds them.
#[derive(Debug, Clone, Copy)]
pub struct LabelContext<'a> {
    /// The asset's sidecar.
    pub sidecar: &'a SidecarV1,
    /// The stack record for the stack the asset belongs to. Ignored unless the sidecar's
    /// current `stack_membership` names this record's `stack_id`.
    pub stack: Option<&'a StackDocV1>,
    /// The record of the asset's container album.
    pub album: Option<&'a AlbumDocV1>,
}

fn has(set: &OrSet<String>, label: &str) -> bool {
    set.value().iter().any(|l| l == label)
}

impl LabelContext<'_> {
    /// The stack record, when the asset is a current member of it.
    fn member_stack(&self) -> Option<&StackDocV1> {
        let stack = self.stack?;
        let membership = self.sidecar.stack_membership.get()?.as_ref()?;
        (membership.stack_id == stack.stack_id).then_some(stack)
    }

    /// The first level, in [`LabelLevel`] order, at which `label` is attached to the asset, or
    /// `None` when no level carries it.
    pub fn level_of(&self, label: &str) -> Option<LabelLevel> {
        let level = if has(&self.sidecar.tags_user, label) {
            Some(LabelLevel::Asset)
        } else if self
            .sidecar
            .regions
            .values()
            .filter_map(live_region)
            .any(|region| has(&region.labels, label))
        {
            Some(LabelLevel::Region)
        } else if self.member_stack().is_some_and(|s| has(&s.labels, label)) {
            Some(LabelLevel::Stack)
        } else if self.album.is_some_and(|a| has(&a.labels, label)) {
            Some(LabelLevel::Album)
        } else {
            None
        };
        tracing::trace!(asset = %self.sidecar.uuid, ?level, "smart-album tags_user lookup");
        level
    }

    /// Whether a `tags_user` `contains` term with operand `label` matches this asset.
    pub fn tags_user_contains(&self, label: &str) -> bool {
        self.level_of(label).is_some()
    }
}

#[cfg(test)]
mod tests;
