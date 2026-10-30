use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use capsule_core::crypto::keys::ClassicalAlgorithm;
use tempfile::TempDir;

use super::*;

/// An in-memory P-256 secure element with the semantics the Android `StrongBoxSigner` and the
/// Secure Enclave signer have: `enroll` mints a key under an alias the first time and returns the
/// same key after, `classical_public_key` reports [`HardwareSignerError::NotFound`] for an alias
/// it holds no key under, and every call fails with [`HardwareSignerError::Unavailable`] while
/// the element is switched off.
pub(crate) struct MockP256Signer {
    keys: Mutex<HashMap<String, p256::ecdsa::SigningKey>>,
    next_scalar: AtomicU8,
    unavailable: AtomicBool,
}

impl MockP256Signer {
    /// An element whose first minted key derives from `scalar`.
    pub(crate) fn new(scalar: u8) -> Self {
        Self {
            keys: Mutex::new(HashMap::new()),
            next_scalar: AtomicU8::new(scalar),
            unavailable: AtomicBool::new(false),
        }
    }

    /// Drop the key under `alias`, as a cleared keystore or a factory reset does.
    pub(crate) fn forget(&self, alias: &str) {
        self.keys.lock().unwrap().remove(alias);
    }

    /// Switch the element off (`true`) or back on.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    /// How many keys the element holds.
    pub(crate) fn key_count(&self) -> usize {
        self.keys.lock().unwrap().len()
    }

    fn guard(&self) -> std::result::Result<(), HardwareSignerError> {
        if self.unavailable.load(Ordering::SeqCst) {
            Err(HardwareSignerError::Unavailable)
        } else {
            Ok(())
        }
    }
}

impl HardwareSigner for MockP256Signer {
    fn enroll(&self, key_alias: String) -> std::result::Result<Vec<u8>, HardwareSignerError> {
        self.guard()?;
        {
            let mut keys = self.keys.lock().unwrap();
            if !keys.contains_key(&key_alias) {
                let scalar = self.next_scalar.fetch_add(1, Ordering::SeqCst);
                let key = p256::ecdsa::SigningKey::from_slice(&[scalar; 32]).unwrap();
                keys.insert(key_alias.clone(), key);
            }
        }
        self.classical_public_key(key_alias)
    }

    fn classical_public_key(
        &self,
        key_alias: String,
    ) -> std::result::Result<Vec<u8>, HardwareSignerError> {
        self.guard()?;
        let keys = self.keys.lock().unwrap();
        let key = keys.get(&key_alias).ok_or(HardwareSignerError::NotFound)?;
        Ok(key
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec())
    }

    fn sign_classical(
        &self,
        key_alias: String,
        msg: Vec<u8>,
    ) -> std::result::Result<Vec<u8>, HardwareSignerError> {
        use p256::ecdsa::signature::Signer as _;
        self.guard()?;
        let keys = self.keys.lock().unwrap();
        let key = keys.get(&key_alias).ok_or(HardwareSignerError::NotFound)?;
        let sig: p256::ecdsa::Signature = key.sign(&msg);
        Ok(sig.to_der().as_bytes().to_vec())
    }

    fn assert_non_exportable(
        &self,
        _key_alias: String,
    ) -> std::result::Result<(), HardwareSignerError> {
        self.guard()
    }
}

const ALIAS: &str = "capsule-device-dsk";

fn binding(element: &Arc<MockP256Signer>) -> HardwareSignerBinding {
    HardwareSignerBinding {
        element: element.clone(),
        key_alias: ALIAS.into(),
    }
}

fn library_root() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join(".library")).unwrap();
    dir
}

#[test]
fn an_enrolled_record_reattaches_to_the_same_published_key() {
    let element = Arc::new(MockP256Signer::new(3));
    let master = MasterKey::generate();
    let (signer, record) =
        DeviceSignerRecord::enroll(binding(&element), &[9; 32], &master).unwrap();
    assert_eq!(
        signer.verifying_key().classical_algorithm(),
        ClassicalAlgorithm::EcdsaP256
    );

    let root = library_root();
    record.persist(root.path()).unwrap();
    let loaded = DeviceSignerRecord::load(root.path()).unwrap().unwrap();
    assert_eq!(loaded, record);

    let reattached = loaded.reattach(&master, element.clone()).unwrap();
    assert_eq!(reattached.verifying_key(), signer.verifying_key());
    let sig = reattached.sign(b"manifest").unwrap();
    assert!(signer.verifying_key().verify(b"manifest", &sig));
}

#[test]
fn the_seed_is_sealed_rather_than_written_in_the_clear() {
    let element = Arc::new(MockP256Signer::new(3));
    let master = MasterKey::generate();
    let seed = [0xA5; 32];
    let (_, record) = DeviceSignerRecord::enroll(binding(&element), &seed, &master).unwrap();
    let root = library_root();
    record.persist(root.path()).unwrap();
    let bytes = fs::read(DeviceSignerRecord::path(root.path())).unwrap();
    assert!(
        !bytes.windows(seed.len()).any(|w| w == seed),
        "the ML-DSA seed must not appear in the record's bytes"
    );
}

#[test]
fn a_library_without_a_record_loads_none() {
    let root = library_root();
    assert!(DeviceSignerRecord::load(root.path()).unwrap().is_none());
}

#[test]
fn an_undecodable_or_unknown_version_record_is_an_error_not_an_absence() {
    let root = library_root();
    fs::write(DeviceSignerRecord::path(root.path()), b"not cbor").unwrap();
    assert!(matches!(
        DeviceSignerRecord::load(root.path()),
        Err(LifecycleError::Cbor(_))
    ));

    let element = Arc::new(MockP256Signer::new(3));
    let (_, mut record) =
        DeviceSignerRecord::enroll(binding(&element), &[9; 32], &MasterKey::generate()).unwrap();
    record.version = 2;
    record.persist(root.path()).unwrap();
    assert!(matches!(
        DeviceSignerRecord::load(root.path()),
        Err(LifecycleError::Cbor(_))
    ));
}

#[test]
fn reattaching_never_mints_a_key_the_element_lost() {
    let element = Arc::new(MockP256Signer::new(3));
    let master = MasterKey::generate();
    let (_, record) = DeviceSignerRecord::enroll(binding(&element), &[9; 32], &master).unwrap();
    element.forget(ALIAS);

    assert!(matches!(
        record.reattach(&master, element.clone()),
        Err(LifecycleError::Crypto(CryptoError::Key(_)))
    ));
    assert_eq!(element.key_count(), 0, "no replacement key was minted");
}

#[test]
fn reattaching_to_another_key_under_the_alias_is_refused() {
    let element = Arc::new(MockP256Signer::new(3));
    let master = MasterKey::generate();
    let (_, record) = DeviceSignerRecord::enroll(binding(&element), &[9; 32], &master).unwrap();
    // The alias now names a different key: the old one was cleared and a new one minted.
    element.forget(ALIAS);
    element.enroll(ALIAS.into()).unwrap();

    assert!(matches!(
        record.reattach(&master, element),
        Err(LifecycleError::Crypto(CryptoError::Key(_)))
    ));
}

#[test]
fn reattaching_under_another_master_key_is_refused() {
    let element = Arc::new(MockP256Signer::new(3));
    let (_, record) =
        DeviceSignerRecord::enroll(binding(&element), &[9; 32], &MasterKey::generate()).unwrap();
    assert!(matches!(
        record.reattach(&MasterKey::generate(), element),
        Err(LifecycleError::Crypto(CryptoError::Auth(_)))
    ));
}

#[test]
fn an_unavailable_element_refuses_enrollment() {
    let element = Arc::new(MockP256Signer::new(3));
    element.set_unavailable(true);
    assert!(matches!(
        DeviceSignerRecord::enroll(binding(&element), &[9; 32], &MasterKey::generate()),
        Err(LifecycleError::Crypto(CryptoError::Key(_)))
    ));
}

#[test]
fn removing_a_stale_record_is_idempotent() {
    let root = library_root();
    let element = Arc::new(MockP256Signer::new(3));
    let (_, record) =
        DeviceSignerRecord::enroll(binding(&element), &[9; 32], &MasterKey::generate()).unwrap();
    record.persist(root.path()).unwrap();
    DeviceSignerRecord::remove_stale(root.path()).unwrap();
    assert!(!DeviceSignerRecord::path(root.path()).exists());
    DeviceSignerRecord::remove_stale(root.path()).unwrap();
}
