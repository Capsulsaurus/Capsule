use super::*;

fn snapshot() -> FfiPathSnapshot {
    FfiPathSnapshot {
        cost: FfiPathCost::Unmetered,
        reachable: true,
        interface: FfiPathInterface::Other,
        power: FfiPowerState::External,
        thermal: FfiThermalState::Nominal,
        background_deadline_ms: None,
    }
}

#[test]
fn the_desktop_snapshot_is_the_desktop_sample() {
    assert_eq!(PathSample::from(snapshot()), PathSample::desktop());
}

#[test]
fn every_field_maps_onto_the_sample() {
    let sample = PathSample::from(FfiPathSnapshot {
        cost: FfiPathCost::Constrained,
        reachable: false,
        interface: FfiPathInterface::Cellular,
        power: FfiPowerState::LowPower,
        thermal: FfiThermalState::Serious,
        background_deadline_ms: Some(30_000),
    });
    assert_eq!(
        sample,
        PathSample {
            cost: PathCost::Constrained,
            reachability: Reachability::Unreachable,
            interface: PathInterface::Cellular,
            power: PowerState::LowPower,
            thermal: ThermalState::Serious,
            execution: Execution::Background {
                deadline: Duration::from_secs(30),
            },
        }
    );

    let rest = [
        (FfiPathCost::Metered, PathCost::Metered),
        (FfiPathCost::Unmetered, PathCost::Unmetered),
    ];
    for (ffi, cost) in rest {
        assert_eq!(
            PathSample::from(FfiPathSnapshot {
                cost: ffi,
                ..snapshot()
            })
            .cost,
            cost
        );
    }
    for (ffi, interface) in [
        (FfiPathInterface::Wired, PathInterface::Wired),
        (FfiPathInterface::Wifi, PathInterface::Wifi),
    ] {
        assert_eq!(
            PathSample::from(FfiPathSnapshot {
                interface: ffi,
                ..snapshot()
            })
            .interface,
            interface
        );
    }
    assert_eq!(
        PathSample::from(FfiPathSnapshot {
            power: FfiPowerState::Battery,
            ..snapshot()
        })
        .power,
        PowerState::Battery
    );
    for (ffi, thermal) in [
        (FfiThermalState::Fair, ThermalState::Fair),
        (FfiThermalState::Critical, ThermalState::Critical),
    ] {
        assert_eq!(
            PathSample::from(FfiPathSnapshot {
                thermal: ffi,
                ..snapshot()
            })
            .thermal,
            thermal
        );
    }
}

#[test]
fn only_link_failures_are_link_signals() {
    assert_eq!(
        link_signal(&Ok(AssetPushReport::default())),
        Some(TransferSignal::Clean)
    );
    assert_eq!(
        link_signal(&Err(PushError::Upload(UploadError::Transport(
            "reset".into()
        )))),
        Some(TransferSignal::Reset)
    );
    assert_eq!(
        link_signal(&Err(PushError::Upload(UploadError::RetriesExhausted(
            "stuck".into()
        )))),
        Some(TransferSignal::Stall)
    );
    assert_eq!(
        link_signal(&Err(PushError::Upload(UploadError::Rejected {
            status: 403,
            code: None,
            message: "no".into(),
        }))),
        None
    );
}
