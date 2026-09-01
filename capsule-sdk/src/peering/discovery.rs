//! LAN discovery: the opaque, rotating advertisement and the [`Discovery`] seam.
//!
//! Discovery is the one genuinely new mechanism peering introduces. Devices advertise a
//! peering service over **mDNS** and accept connections over **TCP**. mDNS broadcasts are
//! visible to every host on the segment, so the advertisement **must not leak identity**: a
//! device advertises an [`OpaqueAdvertisement`] — a rotated opaque instance name — never
//! `user@server.tld` or a device name. Whether two advertisements belong to the same user is
//! established *inside* the encrypted [`channel`](super::channel), never from the broadcast.
//!
//! The live mDNS responder is intentionally **not** built into this slice: it is inherently
//! non-deterministic (real multicast, real timing) and belongs behind the [`Discovery`] seam,
//! exactly as the connection-class detector kept live NIC probing behind a signal seam. Tests
//! drive a [`MockDiscovery`]; a production responder — pure-Rust `mdns-sd` is the sanctioned
//! choice — implements [`Discovery`] over the same [`OpaqueAdvertisement`] descriptor.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use tracing::instrument;

use super::PeeringError;

/// The mDNS service type peering advertises under. A fixed, non-identifying label shared by
/// every Capsule device — it says "a Capsule peer is here", nothing about *who*.
pub const SERVICE_TYPE: &str = "_capsule-peer._tcp.local.";

/// The opaque instance name length in bytes (rendered as `2 * OPAQUE_BYTES` hex chars). 128
/// bits is ample to make two devices' names collision-free on a LAN while carrying no structure
/// an observer could decode into an identity.
const OPAQUE_BYTES: usize = 16;

/// Seconds in a day — the ceiling of the advertisement rotation band.
const DAY_SECS: i64 = 86_400;

/// A LAN peering advertisement: an **opaque, rotating** service-instance name plus the TCP port
/// the device listens on. The name is derived purely from a per-device rotation secret and a
/// rotation epoch, so structurally there is no user handle or device name to leak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpaqueAdvertisement {
    name: String,
    port: u16,
}

impl OpaqueAdvertisement {
    /// Derive the advertisement for a rotation `epoch` (see [`rotation_epoch`]). The instance
    /// name is `hex(SHA-256("capsule-peer-advert-v1" ‖ rotation_seed ‖ epoch)[..16])` — a
    /// one-way function of a secret the peer never broadcasts, so the same epoch reproduces the
    /// same opaque name and a new epoch yields an unlinkable new one.
    #[must_use]
    pub fn for_epoch(rotation_seed: &[u8; 32], epoch: u64, port: u16) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"capsule-peer-advert-v1");
        hasher.update(rotation_seed);
        hasher.update(epoch.to_le_bytes());
        let digest = hasher.finalize();
        Self {
            name: hex::encode(&digest[..OPAQUE_BYTES]),
            port,
        }
    }

    /// The opaque service-instance name that goes on the wire. Carries no identity.
    #[must_use]
    pub fn advertised_name(&self) -> &str {
        &self.name
    }

    /// The fixed, non-identifying mDNS service type.
    #[must_use]
    pub fn service_type(&self) -> &'static str {
        SERVICE_TYPE
    }

    /// The TCP port the peering listener is bound to.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }
}

/// Compute the rotation epoch from a per-boot identifier and the current UTC time.
///
/// The peering doc requires the opaque advertisement to rotate **at least per boot and at most
/// every 24 h**. This returns a value that (a) changes whenever `boot_id` changes — satisfying
/// "at least per boot" — and (b) within a single boot changes only at a UTC-day boundary —
/// satisfying "at most every 24 h". Multiplying `boot_id` by an odd constant is a bijection, so
/// distinct boots map to distinct epoch families; the day bucket advances it once per day.
#[must_use]
pub fn rotation_epoch(boot_id: u64, unix_secs: i64) -> u64 {
    let day_bucket = unix_secs.div_euclid(DAY_SECS) as u64;
    boot_id
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(day_bucket)
}

/// A peer found on the LAN: its opaque instance name and the socket to dial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPeer {
    /// The peer's opaque mDNS instance name (non-identifying).
    pub instance_name: String,
    /// The address to open the peering [`channel`](super::channel) to.
    pub addr: SocketAddr,
}

/// The LAN discovery seam. Abstracted so the (non-deterministic, multicast) mDNS responder is
/// mocked in tests while a production `mdns-sd`-backed implementation plugs in unchanged. A
/// device both **advertises** its own opaque service and **browses** for peers; peering is
/// best-effort, so a browse that finds nothing simply returns an empty list.
pub trait Discovery {
    /// Publish `advert` for `addr` on the LAN. Idempotent — re-advertising a rotated name
    /// replaces the previous one.
    fn advertise(&self, advert: &OpaqueAdvertisement, addr: SocketAddr)
    -> Result<(), PeeringError>;

    /// Browse the LAN for currently-advertised peers. Never blocks indefinitely and never errors
    /// on "no peers" — an empty vector means fall back to server sync.
    fn browse(&self) -> Result<Vec<DiscoveredPeer>, PeeringError>;
}

/// A deterministic in-memory [`Discovery`] for tests: a shared registry of advertised peers.
/// Cloning shares the registry, so a test can advertise on one handle and browse on another —
/// standing in for two devices on the same segment.
#[derive(Debug, Clone, Default)]
pub struct MockDiscovery {
    registry: Arc<Mutex<Vec<DiscoveredPeer>>>,
}

impl MockDiscovery {
    /// A fresh, empty discovery registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Discovery for MockDiscovery {
    #[instrument(skip(self), fields(name = advert.advertised_name(), %addr))]
    fn advertise(
        &self,
        advert: &OpaqueAdvertisement,
        addr: SocketAddr,
    ) -> Result<(), PeeringError> {
        let mut reg = self
            .registry
            .lock()
            .map_err(|_| PeeringError::Discovery("registry poisoned".into()))?;
        let peer = DiscoveredPeer {
            instance_name: advert.advertised_name().to_string(),
            addr,
        };
        // Rotated re-advertise: replace any prior entry for this exact name.
        reg.retain(|p| p.instance_name != peer.instance_name);
        tracing::debug!("advertising opaque peering service on the LAN");
        reg.push(peer);
        Ok(())
    }

    fn browse(&self) -> Result<Vec<DiscoveredPeer>, PeeringError> {
        let reg = self
            .registry
            .lock()
            .map_err(|_| PeeringError::Discovery("registry poisoned".into()))?;
        Ok(reg.clone())
    }
}
