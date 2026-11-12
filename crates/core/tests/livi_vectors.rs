// SPDX-FileCopyrightText: 2025-2026 Lasse Heitgres
// SPDX-License-Identifier: GPL-3.0-or-later
// LIVI's synthetic CSM vectors; source and attribution: research/livi-csm-lock.json, NOTICE.md.
use f50_core::{
    framing::{params, Csm, CsmDecoder},
    mfi::Identity,
    vehicle::Values,
    vehicle_status::VehicleStatus,
    wireless::{Endpoint, Wireless},
};

fn vectors() -> Vec<(&'static str, Vec<u8>)> {
    include_str!("data/livi-csm-vectors.txt")
        .lines()
        .map(|line| {
            let (name, hex) = line.split_once(' ').unwrap();
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            (name, bytes)
        })
        .collect()
}
fn vector(name: &str) -> Csm {
    let bytes = vectors().into_iter().find(|(n, _)| *n == name).unwrap().1;
    let mut decoded = Vec::new();
    CsmDecoder::default()
        .feed(&bytes, |message| decoded.push(message))
        .unwrap();
    decoded.remove(0)
}

#[test]
fn upstream_csm_vectors_survive_fragmentation_and_roundtrip() {
    for (name, bytes) in vectors() {
        for size in [1, 3, 17, bytes.len()] {
            let mut decoder = CsmDecoder::default();
            let mut messages = Vec::new();
            for fragment in bytes.chunks(size) {
                decoder
                    .feed(fragment, |message| messages.push(message))
                    .unwrap();
            }
            decoder.eof().unwrap();
            assert_eq!(messages.len(), 1, "{name} chunk={size}");
            assert_eq!(messages[0].encode().unwrap(), bytes, "{name}");
            params(&messages[0].body).unwrap();
        }
    }
}

struct SyntheticIdentity;
impl Identity for SyntheticIdentity {
    fn certificate(&self) -> &[u8] {
        &[1, 2, 255]
    }
    fn sign_digest(&self, _: &[u8; 32]) -> f50_core::Result<[u8; 64]> {
        Ok([0; 64])
    }
}

#[test]
fn authenticated_wifi_handoff_matches_upstream_wire_fields() {
    let mut endpoint = Endpoint {
        name: "LIVI".into(),
        model: "synthetic".into(),
        manufacturer: "test".into(),
        serial: "test".into(),
        bluetooth_mac: [2, 0, 0, 0, 0, 1],
        ssid: "LIVI".into(),
        password: "secret123".to_owned().into(),
        channel: 36,
        security: 2,
        addresses: vec!["192.168.2.1".parse().unwrap()],
        airplay_port: 49152,
        pairing_public_key: [0; 32],
        source_version: "1.0".into(),
    };
    let mut wireless = Wireless::new(0);
    let identity = SyntheticIdentity;
    assert!(wireless
        .handle(
            &Csm {
                id: 0x5702,
                body: vec![]
            },
            &endpoint,
            &identity,
            0
        )
        .is_err());
    let mut wireless = Wireless::new(0);
    wireless
        .handle(
            &Csm {
                id: 0x1d00,
                body: vec![],
            },
            &endpoint,
            &identity,
            0,
        )
        .unwrap();
    wireless
        .handle(&vector("IdentificationAccepted"), &endpoint, &identity, 1)
        .unwrap();
    assert_eq!(
        wireless
            .handle(
                &vector("RequestAuthenticationCertificate"),
                &endpoint,
                &identity,
                2
            )
            .unwrap()
            .remove(0)
            .encode()
            .unwrap(),
        vector("AuthenticationCertificate").encode().unwrap()
    );
    wireless
        .handle(
            &Csm {
                id: 0xaa02,
                body: f50_core::mfi::parameter(0, &[0; 32]).unwrap(),
            },
            &endpoint,
            &identity,
            3,
        )
        .unwrap();
    wireless
        .handle(&vector("AuthenticationSucceeded"), &endpoint, &identity, 4)
        .unwrap();
    let wifi = wireless
        .handle(
            &Csm {
                id: 0x5702,
                body: vec![],
            },
            &endpoint,
            &identity,
            5,
        )
        .unwrap()
        .remove(0);
    assert_eq!(
        wifi.encode().unwrap(),
        vector("AccessoryWiFiConfigurationInformation")
            .encode()
            .unwrap()
    );
    endpoint.security = 3;
    let started = wireless
        .handle(&vector("CarPlayAvailability"), &endpoint, &identity, 6)
        .unwrap()
        .remove(0);
    let expected = vector("CarPlayStartSession#wireless");
    let actual_fields = params(&started.body).unwrap();
    let expected_fields = params(&expected.body).unwrap();
    for id in [1, 2, 5] {
        assert_eq!(
            actual_fields.iter().find(|(k, _)| *k == id),
            expected_fields.iter().find(|(k, _)| *k == id),
            "handoff field {id}"
        );
    }
}

#[test]
fn ev_range_matches_upstream_and_malformed_tlv_keeps_strict_rejection() {
    let mut status = VehicleStatus::default();
    status
        .set(Values {
            range_km: Some(250.0),
            ..Default::default()
        })
        .unwrap();
    let actual = status
        .handle(&vector("StartVehicleStatusUpdates"))
        .unwrap()
        .unwrap();
    let expected = vector("VehicleStatusUpdate#full");
    assert_eq!(
        params(&actual.body)
            .unwrap()
            .iter()
            .find(|(id, _)| *id == 3),
        params(&expected.body)
            .unwrap()
            .iter()
            .find(|(id, _)| *id == 3)
    );
    let mut malformed = actual.body;
    malformed.push(0);
    assert_eq!(params(&malformed), Err(f50_core::Error::Truncated));
    assert_eq!(
        Csm {
            id: 0xa101,
            body: vec![0; 65530]
        }
        .encode(),
        Err(f50_core::Error::Limit)
    );
}
