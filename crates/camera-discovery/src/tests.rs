use std::collections::BTreeSet;
use std::path::PathBuf;

use super::{
    group_logical_cameras, CameraCandidate, CameraMap, CameraPolicy, MappingError, MappingState,
    Selector, SelectorError,
};

fn candidate(path: &str, parent: &str, serial: &str) -> CameraCandidate {
    CameraCandidate {
        device_path: PathBuf::from(path),
        product_name: "Front Camera".to_owned(),
        vendor_id: Some("1234".to_owned()),
        product_id: Some("5678".to_owned()),
        serial: Some(serial.to_owned()),
        usb_interface: Some("00".to_owned()),
        udev_id_path: Some(parent.to_owned()),
        bus_info: Some(parent.to_owned()),
        media_entity: None,
        endpoint_role: "primary".to_owned(),
        driver: Some("uvcvideo".to_owned()),
        logical_parent: parent.to_owned(),
        capture_capable: true,
        output_capable: false,
        metadata_only: false,
        supported_caps: true,
        managed_virtual_label: false,
    }
}

#[test]
fn stable_id_and_grouping_are_deterministic() {
    let primary = candidate("/dev/video0", "usb-1", "ABC");
    let mut alternate = primary.clone();
    alternate.device_path = PathBuf::from("/dev/video1");
    alternate.endpoint_role = "alternate".to_owned();
    let grouped = group_logical_cameras([alternate, primary.clone()]).expect("group");
    assert_eq!(grouped.len(), 1);
    assert_eq!(
        grouped[0].endpoint.device_path,
        PathBuf::from("/dev/video0")
    );
    assert_eq!(grouped[0].stable_camera_id, primary.stable_camera_id());
}

#[test]
fn generated_loopback_is_excluded() {
    let mut loopback = candidate("/dev/video40", "virtual-40", "virtual");
    loopback.driver = Some("v4l2loopback".to_owned());
    assert!(group_logical_cameras([loopback]).expect("group").is_empty());
}

#[test]
fn selector_precedence_and_invalid_values_fail_closed() {
    let cameras = group_logical_cameras([
        candidate("/dev/video0", "usb-1", "ANCHOR"),
        candidate("/dev/video2", "usb-2", "SECONDARY"),
    ])
    .expect("group");
    let policy = CameraPolicy {
        disable: vec![],
        stream_exclude: vec![Selector::parse("serial:SECONDARY").expect("selector")],
        anchor: Selector::parse("serial:ANCHOR").expect("selector"),
    };
    let result = policy.evaluate(&cameras).expect("policy");
    assert!(
        result
            .iter()
            .find(|item| item.anchor)
            .expect("anchor")
            .stream_enabled
    );
    assert!(
        !result
            .iter()
            .find(|item| item.camera.endpoint.serial.as_deref() == Some("SECONDARY"))
            .expect("secondary")
            .stream_enabled
    );
    assert!(matches!(
        Selector::parse("name_regex:["),
        Err(SelectorError::Regex(_))
    ));
}

#[test]
fn anchor_exclusion_is_a_configuration_error() {
    let cameras =
        group_logical_cameras([candidate("/dev/video0", "usb-1", "ANCHOR")]).expect("group");
    let policy = CameraPolicy {
        disable: vec![],
        stream_exclude: vec![Selector::parse("serial:ANCHOR").expect("selector")],
        anchor: Selector::parse("serial:ANCHOR").expect("selector"),
    };
    assert_eq!(
        policy.evaluate(&cameras).expect_err("conflict"),
        SelectorError::AnchorPolicyConflict
    );
}

#[test]
fn slots_persist_and_tombstones_are_not_reused() {
    let cameras = group_logical_cameras([
        candidate("/dev/video0", "usb-1", "A"),
        candidate("/dev/video2", "usb-2", "B"),
    ])
    .expect("group");
    let mut mapping = CameraMap::default();
    assert_eq!(
        mapping
            .allocate_or_reuse(&cameras[0], 4, 40)
            .expect("slot")
            .stream_slot,
        0
    );
    assert_eq!(
        mapping
            .allocate_or_reuse(&cameras[1], 4, 40)
            .expect("slot")
            .stream_slot,
        1
    );
    mapping.tombstone_missing(&BTreeSet::from([cameras[1].stable_camera_id.clone()]));
    let third = group_logical_cameras([candidate("/dev/video4", "usb-3", "C")])
        .expect("group")
        .remove(0);
    assert_eq!(
        mapping
            .allocate_or_reuse(&third, 4, 40)
            .expect("slot")
            .stream_slot,
        2
    );
    assert_eq!(mapping.entries()[0].state, MappingState::Tombstone);
}

#[test]
fn mapping_persistence_detects_corruption_and_identity_collision() {
    let camera = group_logical_cameras([candidate("/dev/video0", "usb-1", "A")])
        .expect("group")
        .remove(0);
    let mut mapping = CameraMap::default();
    mapping.allocate_or_reuse(&camera, 4, 40).expect("slot");
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("camera-map.json");
    mapping.save_atomic(&path).expect("persist");
    let loaded = CameraMap::load(&path).expect("load");
    assert_eq!(loaded.entries(), mapping.entries());

    std::fs::write(
        &path,
        b"{\"generation\":1,\"entries\":[],\"checksum_blake3\":\"bad\"}",
    )
    .expect("corrupt fixture");
    assert!(matches!(
        CameraMap::load(&path),
        Err(MappingError::Checksum)
    ));

    let mut collision = camera;
    collision.canonical_identity.push_str("changed");
    assert!(matches!(
        mapping.allocate_or_reuse(&collision, 4, 40),
        Err(MappingError::Collision(_))
    ));
}
