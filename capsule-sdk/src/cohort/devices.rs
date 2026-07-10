//! The devices-grouping view model and the one-tap support bundle (slice `S-D11`).
//!
//! Consumes the server's `GET /devices` `{devices, cohorts}` body (slice `S-C13`)
//! into [`DevicesView`] — the session ledger grouped by device cohort so one physical
//! device's re-enrollments collapse into a single row the UI renders. The copy is
//! **assert-don't-litigate**: a group is labelled "a device you've used before (last
//! seen …)" with no "this isn't my device" toggle, because the user cannot adjudicate
//! an advisory hash. The only dispute path is [`SupportBundle`] — the exact hash and
//! device/session map, bundled for a support report.
//!
//! All copy lives as `locales/` catalog **keys** ([`keys`]); the view model exposes
//! the key plus the structured data (timestamps), and the platform app resolves the
//! key through `capsule-i18n`. This module never formats a human string.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Catalog keys for the devices-grouping UI (namespace `device.cohort.*`). Assert,
/// do not litigate — the copy states recognition, it never asks the user to arbitrate.
pub mod keys {
    /// Label for a cohort seen before this session (reinstall / an earlier expired
    /// session). Takes a formatted `{last_seen}` date the app supplies.
    pub const LABEL_PREVIOUSLY_USED: &str = "device.cohort.label.previously_used";
    /// Label for the session currently making the request.
    pub const LABEL_THIS_DEVICE: &str = "device.cohort.label.this_device";
    /// Label for a device newly seen on this account.
    pub const LABEL_NEW_DEVICE: &str = "device.cohort.label.new_device";
    /// The one-tap support action that emits a [`super::SupportBundle`].
    pub const SUPPORT_REPORT: &str = "device.cohort.support.report";
}

// ─── Wire types (GET /devices) ───────────────────────────────────────────────

/// One active session ("device") from the session listing. Mirrors the server's
/// `Device` (slice `S-C13`); `id` is the session id, `cohort_hash` its advisory
/// grouping value (absent when the client asserted none).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceEntry {
    /// The session id.
    pub id: String,
    /// Session creation time (Unix seconds).
    pub created_at: i64,
    /// Last activity time (Unix seconds).
    pub last_active_at: i64,
    /// Reported user agent, if any.
    #[serde(default)]
    pub user_agent: Option<String>,
    /// Reported source IP, if any.
    #[serde(default)]
    pub ip_address: Option<String>,
    /// Whether this is the session making the current request.
    pub is_current: bool,
    /// The advisory device-cohort hash asserted at session creation, if any.
    #[serde(default)]
    pub cohort_hash: Option<String>,
}

/// One entry of the durable cohort map (persists beyond session expiry). Mirrors the
/// server's `DeviceCohort`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortMapEntry {
    /// The advisory cohort hash.
    pub cohort_hash: String,
    /// First time this cohort was seen for the user (Unix seconds).
    pub first_seen: i64,
    /// Most recent time this cohort was seen for the user (Unix seconds).
    pub last_seen: i64,
}

/// The `GET /devices` body: active sessions plus the durable cohort map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionListing {
    /// Active sessions, each annotated with its per-session cohort.
    pub devices: Vec<DeviceEntry>,
    /// The durable cohort map.
    pub cohorts: Vec<CohortMapEntry>,
}

// ─── View model ──────────────────────────────────────────────────────────────

/// How the UI should recognize a cohort group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recognition {
    /// First appearance of this cohort on the account.
    New,
    /// Seen before — a reinstall (new `device_id`, same cohort) or an earlier session
    /// that has since expired. The "previously used" state.
    PreviouslyUsed,
}

impl Recognition {
    /// The `locales/` catalog key for this recognition state.
    pub fn label_key(self) -> &'static str {
        match self {
            Recognition::New => keys::LABEL_NEW_DEVICE,
            Recognition::PreviouslyUsed => keys::LABEL_PREVIOUSLY_USED,
        }
    }
}

/// One grouped device: every session that shares a cohort hash, collapsed into a
/// single row with its recognition state and the durable first/last-seen span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CohortGroup {
    /// The shared cohort hash, or `None` for an ungrouped session (the client
    /// asserted no cohort — it cannot be grouped and stands alone).
    pub cohort_hash: Option<String>,
    /// The sessions in this group, oldest first.
    pub sessions: Vec<DeviceEntry>,
    /// First time this cohort was seen (durable map, else derived from sessions).
    pub first_seen: i64,
    /// Most recent time this cohort was seen (durable map, else derived from sessions).
    pub last_seen: i64,
    /// The recognition state driving the group's label.
    pub recognition: Recognition,
    /// Whether the current session is in this group ("this device").
    pub contains_current: bool,
}

impl CohortGroup {
    /// The catalog key the UI renders for this group. The current device asserts
    /// "this device"; otherwise the recognition state decides.
    pub fn label_key(&self) -> &'static str {
        if self.contains_current {
            keys::LABEL_THIS_DEVICE
        } else {
            self.recognition.label_key()
        }
    }

    /// Build the one-tap support bundle for this group: the exact cohort hash and the
    /// per-session `(device_id, session_id, first_seen, last_seen)` map, for a support
    /// report. The dispute path — the client asserts, it does not litigate.
    pub fn support_bundle(&self) -> SupportBundle {
        SupportBundle {
            cohort_hash: self.cohort_hash.clone(),
            sessions: self
                .sessions
                .iter()
                .map(|d| SupportBundleEntry {
                    // The S-C13 listing surfaces only the session id; the hardware
                    // `device_id` is not yet on this wire surface (server follow-up).
                    device_id: None,
                    session_id: d.id.clone(),
                    first_seen: d.created_at,
                    last_seen: d.last_active_at,
                })
                .collect(),
        }
    }
}

/// The devices ledger grouped by cohort — what the devices screen renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevicesView {
    /// The cohort groups, current device first, then most-recently-seen first.
    pub groups: Vec<CohortGroup>,
}

impl DevicesView {
    /// Group a `GET /devices` listing into the view model.
    ///
    /// Sessions sharing a cohort hash collapse into one group; a session with no
    /// cohort stands alone (it cannot be grouped). A group is [`Recognition::PreviouslyUsed`]
    /// when it holds more than one session (a reinstall re-enrolled) **or** the durable
    /// map's `first_seen` predates its earliest current session (an earlier session has
    /// since expired) — otherwise it is [`Recognition::New`].
    pub fn from_listing(listing: &SessionListing) -> Self {
        let durable: BTreeMap<&str, &CohortMapEntry> = listing
            .cohorts
            .iter()
            .map(|c| (c.cohort_hash.as_str(), c))
            .collect();

        // Grouped sessions, keyed by hash for deterministic ordering.
        let mut grouped: BTreeMap<String, Vec<DeviceEntry>> = BTreeMap::new();
        let mut ungrouped: Vec<DeviceEntry> = Vec::new();
        for device in &listing.devices {
            match &device.cohort_hash {
                Some(hash) => grouped
                    .entry(hash.clone())
                    .or_default()
                    .push(device.clone()),
                None => ungrouped.push(device.clone()),
            }
        }

        let mut groups: Vec<CohortGroup> = Vec::new();

        for (hash, mut sessions) in grouped {
            sessions.sort_by_key(|d| (d.created_at, d.id.clone()));
            let earliest_created = sessions.iter().map(|d| d.created_at).min().unwrap_or(0);
            let contains_current = sessions.iter().any(|d| d.is_current);
            let durable_entry = durable.get(hash.as_str());

            let (first_seen, last_seen) = match durable_entry {
                Some(e) => (e.first_seen, e.last_seen),
                None => (
                    earliest_created,
                    sessions.iter().map(|d| d.last_active_at).max().unwrap_or(0),
                ),
            };

            let previously_used = sessions.len() > 1
                || durable_entry.is_some_and(|e| e.first_seen < earliest_created);
            let recognition = if previously_used {
                Recognition::PreviouslyUsed
            } else {
                Recognition::New
            };

            groups.push(CohortGroup {
                cohort_hash: Some(hash),
                sessions,
                first_seen,
                last_seen,
                recognition,
                contains_current,
            });
        }

        // Each cohort-less session stands alone: New, never grouped.
        for device in ungrouped {
            groups.push(CohortGroup {
                cohort_hash: None,
                first_seen: device.created_at,
                last_seen: device.last_active_at,
                contains_current: device.is_current,
                recognition: Recognition::New,
                sessions: vec![device],
            });
        }

        // Current device first, then most-recently-seen first; hash breaks ties.
        groups.sort_by(|a, b| {
            b.contains_current
                .cmp(&a.contains_current)
                .then(b.last_seen.cmp(&a.last_seen))
                .then(a.cohort_hash.cmp(&b.cohort_hash))
        });

        Self { groups }
    }

    /// The group containing the current session, if any.
    pub fn current(&self) -> Option<&CohortGroup> {
        self.groups.iter().find(|g| g.contains_current)
    }
}

// ─── Support bundle ──────────────────────────────────────────────────────────

/// One session's row in a [`SupportBundle`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportBundleEntry {
    /// The hardware `device_id`, when the surface provides it. `None` today — the
    /// S-C13 listing exposes only the session id (a server follow-up owes device_id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// The session id.
    pub session_id: String,
    /// This session's creation time (Unix seconds).
    pub first_seen: i64,
    /// This session's last activity time (Unix seconds).
    pub last_seen: i64,
}

/// The one-tap dispute payload: the exact advisory cohort hash and the device/session
/// map for a support report. Serializable and round-trippable so a client can attach
/// it to a bug report verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportBundle {
    /// The advisory cohort hash under dispute (`None` for an ungrouped session).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cohort_hash: Option<String>,
    /// The sessions attributed to this cohort.
    pub sessions: Vec<SupportBundleEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(
        id: &str,
        cohort: Option<&str>,
        created: i64,
        last: i64,
        current: bool,
    ) -> DeviceEntry {
        DeviceEntry {
            id: id.to_string(),
            created_at: created,
            last_active_at: last,
            user_agent: None,
            ip_address: None,
            is_current: current,
            cohort_hash: cohort.map(str::to_string),
        }
    }

    #[test]
    fn reinstall_two_sessions_one_cohort_group_previously_used() {
        // A reinstall: a new device_id (new session) with the SAME cohort hash. The
        // two sessions must collapse into ONE group in the "previously used" state.
        let listing = SessionListing {
            devices: vec![
                device("sess-old", Some("cohortA"), 100, 150, false),
                device("sess-new", Some("cohortA"), 200, 250, true),
            ],
            cohorts: vec![CohortMapEntry {
                cohort_hash: "cohortA".into(),
                first_seen: 100,
                last_seen: 250,
            }],
        };
        let view = DevicesView::from_listing(&listing);
        assert_eq!(view.groups.len(), 1, "one physical device ⇒ one group");
        let g = &view.groups[0];
        assert_eq!(g.cohort_hash.as_deref(), Some("cohortA"));
        assert_eq!(g.sessions.len(), 2);
        assert_eq!(g.recognition, Recognition::PreviouslyUsed);
        assert!(g.contains_current);
        // The current session asserts "this device".
        assert_eq!(g.label_key(), keys::LABEL_THIS_DEVICE);
        // The recognition itself (ignoring current-device badge) is "previously used".
        assert_eq!(g.recognition.label_key(), keys::LABEL_PREVIOUSLY_USED);
        assert_eq!(g.first_seen, 100);
        assert_eq!(g.last_seen, 250);
    }

    #[test]
    fn durable_map_outlives_session_previously_used_from_single_session() {
        // Only one live session, but the durable map remembers an earlier (expired)
        // one via an older first_seen ⇒ "previously used".
        let listing = SessionListing {
            devices: vec![device("sess-1", Some("cohortB"), 500, 550, true)],
            cohorts: vec![CohortMapEntry {
                cohort_hash: "cohortB".into(),
                first_seen: 100, // older than the live session's creation
                last_seen: 550,
            }],
        };
        let view = DevicesView::from_listing(&listing);
        let g = view.current().unwrap();
        assert_eq!(g.recognition, Recognition::PreviouslyUsed);
        assert_eq!(g.first_seen, 100);
    }

    #[test]
    fn fresh_single_session_is_new() {
        let listing = SessionListing {
            devices: vec![device("sess-1", Some("cohortC"), 100, 120, true)],
            cohorts: vec![CohortMapEntry {
                cohort_hash: "cohortC".into(),
                first_seen: 100,
                last_seen: 120,
            }],
        };
        let view = DevicesView::from_listing(&listing);
        let g = view.current().unwrap();
        assert_eq!(g.recognition, Recognition::New);
        assert_eq!(g.label_key(), keys::LABEL_THIS_DEVICE); // current badge
        assert_eq!(g.recognition.label_key(), keys::LABEL_NEW_DEVICE);
    }

    #[test]
    fn cohortless_sessions_stand_alone() {
        let listing = SessionListing {
            devices: vec![
                device("no-cohort-1", None, 100, 110, true),
                device("no-cohort-2", None, 200, 210, false),
            ],
            cohorts: vec![],
        };
        let view = DevicesView::from_listing(&listing);
        assert_eq!(view.groups.len(), 2, "ungrouped sessions never merge");
        assert!(view.groups.iter().all(|g| g.cohort_hash.is_none()));
        // Current device sorts first.
        assert!(view.groups[0].contains_current);
    }

    #[test]
    fn support_bundle_round_trips() {
        let listing = SessionListing {
            devices: vec![
                device("sess-old", Some("cohortA"), 100, 150, false),
                device("sess-new", Some("cohortA"), 200, 250, true),
            ],
            cohorts: vec![CohortMapEntry {
                cohort_hash: "cohortA".into(),
                first_seen: 100,
                last_seen: 250,
            }],
        };
        let view = DevicesView::from_listing(&listing);
        let bundle = view.current().unwrap().support_bundle();

        assert_eq!(bundle.cohort_hash.as_deref(), Some("cohortA"));
        assert_eq!(bundle.sessions.len(), 2);
        assert_eq!(bundle.sessions[0].session_id, "sess-old");
        assert_eq!(bundle.sessions[0].first_seen, 100);
        assert_eq!(bundle.sessions[0].last_seen, 150);

        // Serializable and round-trippable for attaching to a bug report.
        let json = serde_json::to_string(&bundle).unwrap();
        let back: SupportBundle = serde_json::from_str(&json).unwrap();
        assert_eq!(bundle, back);
        // device_id is absent on this surface today; the field is omitted, not null.
        assert!(!json.contains("device_id"));
    }

    #[test]
    fn cohortless_support_bundle_omits_hash() {
        let listing = SessionListing {
            devices: vec![device("solo", None, 100, 110, true)],
            cohorts: vec![],
        };
        let view = DevicesView::from_listing(&listing);
        let bundle = view.current().unwrap().support_bundle();
        assert!(bundle.cohort_hash.is_none());
        let json = serde_json::to_string(&bundle).unwrap();
        assert!(!json.contains("cohort_hash"));
        let back: SupportBundle = serde_json::from_str(&json).unwrap();
        assert_eq!(bundle, back);
    }
}
