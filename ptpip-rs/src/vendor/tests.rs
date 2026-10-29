use super::*;
use crate::ptp::{ResponseCode, StorageId};
use crate::vendor::sony::{SDIO_CONNECT, SDIO_GET_EXT_DEVICE_INFO, Sony, VENDOR_ID};

/// Records every transaction and answers each with `OK`, or with `refuse` for one operation.
#[derive(Default)]
struct Recorder {
    calls: Vec<(OperationCode, Vec<u32>)>,
    refuse: Option<OperationCode>,
}

impl Transact for Recorder {
    fn transact(
        &mut self,
        operation: OperationCode,
        params: &[u32],
        sink: Option<&mut dyn Write>,
    ) -> Result<Completed, Error> {
        self.calls.push((operation, params.to_vec()));
        if self.refuse == Some(operation) {
            return Err(Error::Response {
                operation,
                code: ResponseCode::GENERAL_ERROR,
            });
        }
        if let Some(sink) = sink {
            sink.write_all(&[0; 8]).map_err(Error::Sink)?;
        }
        Ok(Completed::default())
    }
}

fn device(vendor: u32, manufacturer: &str, ops: &[OperationCode]) -> DeviceInfo {
    DeviceInfo {
        vendor_extension_id: vendor,
        manufacturer: manufacturer.into(),
        model: "body".into(),
        operations_supported: ops.iter().map(|op| op.0).collect(),
        ..DeviceInfo::default()
    }
}

#[test]
fn sony_is_chosen_by_vendor_id_or_manufacturer() {
    assert_eq!(extension_for(&device(VENDOR_ID, "", &[])).name(), "sony");
    assert_eq!(
        extension_for(&device(0x6, "Sony Corporation", &[])).name(),
        "sony"
    );
    assert_eq!(
        extension_for(&device(0x6, "Canon Inc.", &[])).name(),
        "generic"
    );
}

#[test]
fn generic_does_nothing_after_open_session() {
    let mut recorder = Recorder::default();
    Generic
        .after_open_session(&mut recorder, &device(0, "", &[]))
        .unwrap();
    assert!(recorder.calls.is_empty());
    assert_eq!(Generic.operation_name(SDIO_CONNECT), None);
}

#[test]
fn sony_runs_the_three_phase_sdio_handshake_when_advertised() {
    let mut recorder = Recorder::default();
    let body = device(VENDOR_ID, "Sony", &[SDIO_CONNECT, SDIO_GET_EXT_DEVICE_INFO]);
    Sony.after_open_session(&mut recorder, &body).unwrap();
    assert_eq!(
        recorder.calls,
        vec![
            (SDIO_CONNECT, vec![1, 0, 0]),
            (SDIO_CONNECT, vec![2, 0, 0]),
            (SDIO_GET_EXT_DEVICE_INFO, vec![0xC8]),
            (SDIO_CONNECT, vec![3, 0, 0]),
        ]
    );
}

#[test]
fn sony_skips_ext_device_info_when_not_advertised() {
    let mut recorder = Recorder::default();
    Sony.after_open_session(&mut recorder, &device(VENDOR_ID, "Sony", &[SDIO_CONNECT]))
        .unwrap();
    assert_eq!(
        recorder.calls.iter().map(|(op, _)| *op).collect::<Vec<_>>(),
        vec![SDIO_CONNECT, SDIO_CONNECT, SDIO_CONNECT]
    );
}

#[test]
fn sony_without_sdio_connect_gets_no_handshake() {
    let mut recorder = Recorder::default();
    Sony.after_open_session(&mut recorder, &device(VENDOR_ID, "Sony", &[]))
        .unwrap();
    assert!(recorder.calls.is_empty());
}

#[test]
fn a_refused_handshake_step_fails_the_bring_up() {
    let mut recorder = Recorder {
        refuse: Some(SDIO_CONNECT),
        ..Recorder::default()
    };
    let result =
        Sony.after_open_session(&mut recorder, &device(VENDOR_ID, "Sony", &[SDIO_CONNECT]));
    assert!(matches!(result, Err(Error::Response { .. })));
    assert_eq!(recorder.calls.len(), 1);
}

#[test]
fn sony_names_its_operations() {
    assert_eq!(Sony.operation_name(SDIO_CONNECT), Some("SDIOConnect"));
    assert_eq!(Sony.operation_name(OperationCode::GET_OBJECT), None);
    // StorageId formatting is used in the same logs.
    assert_eq!(StorageId(0x0001_0001).to_string(), "00010001");
}
