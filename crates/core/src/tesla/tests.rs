use super::*;
#[test]
fn application_rejection_is_not_protocol_busy() {
    let mut response=proto::nested(1,&[8,1]);
    response.extend(proto::nested(2,&proto::nested(3,&[])));
    assert_eq!(decode_state(Category::Charge,&response,1000),Err(Error::Rejected));
}
#[test]
fn only_read_queries_units_presence_and_per_wheel_times() {
    assert_eq!(Category::Charge.payload(), vec![0x12, 4, 0x0a, 2, 0x12, 0]);
    assert_eq!(Category::Tires.payload(), vec![0x12, 4, 0x0a, 2, 0x72, 0]);
    let empty = decode_state(
        Category::Charge,
        &proto::nested(2, &proto::nested(3, &[])),
        1000,
    )
    .unwrap();
    assert_eq!(
        empty,
        Update::Charge {
            soc_pct: None,
            range_km: None
        }
    );
    let mut fields = Vec::new();
    proto::int(114, 0, &mut fields);
    proto::fixed(111, 100f32.to_bits(), &mut fields);
    let update = decode_state(
        Category::Charge,
        &proto::nested(2, &proto::nested(3, &fields)),
        1000,
    )
    .unwrap();
    if let Update::Charge { soc_pct, range_km } = update {
        assert_eq!(soc_pct.unwrap().value, 0.0);
        assert!((range_km.unwrap().value - 160.9344).abs() < 0.001);
    } else {
        panic!()
    }
    let mut fields = Vec::new();
    proto::fixed(2, 2.5f32.to_bits(), &mut fields);
    proto::fixed(4, 2.6f32.to_bits(), &mut fields);
    let mut at = Vec::new();
    proto::int(1, 1, &mut at);
    proto::bytes(6, &at, &mut fields);
    let update = decode_state(
        Category::Tires,
        &proto::nested(2, &proto::nested(19, &fields)),
        2000,
    )
    .unwrap();
    if let Update::Tires(values) = update {
        assert_eq!(values[0].unwrap().value, 250.0);
        assert_eq!(values[0].unwrap().sampled_ms, Some(1000));
        assert!(values[1].is_none());
        assert!(values[3].is_none());
    } else {
        panic!()
    }
}
#[test]
fn stale_missing_non_finite_and_future_samples_are_not_current_values() {
    let mut t = Telemetry::default();
    t.soc_pct = Some(Reading {
        value: 68.0,
        received_ms: 1000,
        sample_received_ms: 1000,
        sampled_ms: Some(500),
        sampled_time_trusted: true,
    });
    assert_eq!(t.fresh(2000).soc_pct, Some(68.0));
    assert!(t.fresh(32000).soc_pct.is_none());
    t.apply(Update::Charge {
        soc_pct: None,
        range_km: None,
    });
    assert!(t.soc_pct.is_none());
    for value in [f32::NAN, f32::INFINITY, -1.0] {
        let mut f = Vec::new();
        proto::fixed(111, value.to_bits(), &mut f);
        let u = decode_state(
            Category::Charge,
            &proto::nested(2, &proto::nested(3, &f)),
            1000,
        )
        .unwrap();
        assert!(matches!(u, Update::Charge { range_km: None, .. }));
    }
}
#[test]
fn malformed_protobuf_is_bounded() {
    for data in [
        vec![0],
        vec![0x80; 12],
        vec![0x12, 0xff, 0xff, 0xff, 0x7f],
        vec![0x0e],
    ] {
        assert!(Message::parse(&data).is_err());
    }
}
#[test]
fn old_or_unknown_tpms_measurement_time_is_not_relabelled_real_time() {
    for at in [None, Some(1), Some(500000)] {
        let mut fields = Vec::new();
        proto::fixed(2, 2.5f32.to_bits(), &mut fields);
        if let Some(at) = at {
            let mut stamp = Vec::new();
            proto::int(1, at, &mut stamp);
            proto::bytes(6, &stamp, &mut fields);
        }
        let update = decode_state(
            Category::Tires,
            &proto::nested(2, &proto::nested(19, &fields)),
            200000,
        )
        .unwrap();
        let mut telemetry = Telemetry::default();
        telemetry.apply(update);
        assert!(telemetry.fresh(200000).tires_kpa[0].is_none());
        assert_eq!(
            telemetry.tires_kpa[0].unwrap().value,
            250.0,
            "last-known sample remains available in diagnostic state"
        );
    }
}

#[test]
fn climate_queries_and_whitelisted_commands_match_pinned_official_fields() {
    assert_eq!(climate::query(), vec![0x12, 4, 0x0a, 2, 0x1a, 0]);
    assert_eq!(
        ClimateCommand::Power { on: true }.payload().unwrap(),
        vec![0x12, 4, 0x52, 2, 8, 1]
    );
    assert_eq!(
        ClimateCommand::Power { on: false }.payload().unwrap(),
        vec![0x12, 2, 0x52, 0]
    );
    assert_eq!(
        ClimateCommand::SetTemperature {
            driver_celsius: 21.5,
            passenger_celsius: 22.0
        }
        .payload()
        .unwrap(),
        session::tests::hex("1210720e2a021a00350000ac413d0000b041")
    );
    for temperature in [f32::NAN, f32::INFINITY, -1.0, 50.1] {
        for (driver_celsius, passenger_celsius) in [(temperature, 21.0), (21.0, temperature)] {
            assert_eq!(
                ClimateCommand::SetTemperature {
                    driver_celsius,
                    passenger_celsius
                }
                .payload(),
                Err(Error::Malformed)
            );
        }
    }
}

#[test]
fn climate_presence_zero_celsius_limits_and_source_time_are_preserved() {
    let response = |state: &[u8]| {
        let mut response = proto::nested(1, &[]);
        response.extend(proto::nested(2, &proto::nested(4, state)));
        response
    };
    assert_eq!(
        climate::decode(&response(&[])).unwrap(),
        ClimateState::default()
    );
    let mut fields = Vec::new();
    for (field, temperature) in [
        (101, 0.0f32),
        (102, -3.5),
        (103, 21.5),
        (104, 22.0),
        (111, 15.0),
        (112, 28.0),
    ] {
        proto::fixed(field, temperature.to_bits(), &mut fields);
    }
    proto::int(110, 0, &mut fields);
    let mut sampled = Vec::new();
    proto::int(1, 123, &mut sampled);
    proto::int(2, 500_000_000, &mut sampled);
    proto::bytes(33, &sampled, &mut fields);
    let state = climate::decode(&response(&fields)).unwrap();
    assert_eq!(
        state,
        ClimateState {
            hvac_on: Some(false),
            inside_temp_celsius: Some(0.0),
            outside_temp_celsius: Some(-3.5),
            driver_temp_celsius: Some(21.5),
            passenger_temp_celsius: Some(22.0),
            min_temp_celsius: Some(15.0),
            max_temp_celsius: Some(28.0),
            sampled_ms: Some(123_500),
        }
    );
    for temperature in [f32::NAN, f32::INFINITY] {
        let mut fields = Vec::new();
        proto::fixed(101, temperature.to_bits(), &mut fields);
        assert!(climate::decode(&response(&fields))
            .unwrap()
            .inside_temp_celsius
            .is_none());
    }
    assert_eq!(
        climate::decode(&proto::nested(1, &[8, 1])),
        Err(Error::Rejected)
    );
    assert_eq!(
        climate::decode(&proto::nested(1, &[])),
        Err(Error::Malformed)
    );
}
