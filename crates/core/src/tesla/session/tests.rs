use super::*;
pub fn hex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
fn example_identity() -> Identity {
    Identity(
        SecretKey::from_slice(&hex(
            "2538cdc29a97c19c1e99a637d6cf4f8c970c118b56ede1e6323e6d162c4b30db",
        ))
        .unwrap(),
    )
}
#[test]
fn official_ecdh_session_hmac_and_aes_gcm_vectors() {
    let identity = example_identity();
    let remote = hex(
        "04c7a1f47138486aa4729971494878d33b1a24e39571f748a6e16c5955b3d877d3a6aaa0e955166474af5d32c410f439a2234137ad1bb085fd4e8813c958f11d97",
    );
    let key = identity.shared(&remote).unwrap();
    assert_eq!(&key[..], hex("1b2fce19967b79db696f909cff89ea9a"));
    let info = hex(
        "0806124104c7a1f47138486aa4729971494878d33b1a24e39571f748a6e16c5955b3d877d3a6aaa0e955166474af5d32c410f439a2234137ad1bb085fd4e8813c958f11d971a104c463f9cc0d3d26906e982ed224adde6255a0a0000",
    );
    assert_eq!(
        session_tag(
            &key[..],
            b"5YJ30123456789ABC",
            &hex("1588d5a30eabc6f8fc9a951b11f6fd11"),
            &info
        ),
        hex("996c1fe38331be138f8039c194b14db2198846ed7d8251e6749284d7b32ea002")
    );
    let aad = command_aad(
        b"5YJ30123456789ABC",
        &hex("4c463f9cc0d3d26906e982ed224adde6"),
        2655,
        7,
    );
    let (body, tag) = seal(
        &key[..],
        hex("dbf79447fa156674dae1caed").try_into().unwrap(),
        &aad,
        &hex("120452020801"),
    )
    .unwrap();
    assert_eq!(body, hex("38038e8c0f2e"));
    assert_eq!(tag, hex("c228e0ff64991481db3a7bbc133696c5"));
}
pub fn synthetic_session() -> Client {
    let mut c = Client::new("5YJ30123456789ABC", example_identity()).unwrap();
    let _ = c.hello();
    let remote = Identity(SecretKey::from_slice(&[7; 32]).unwrap());
    let key = remote.shared(&c.identity.public_key()).unwrap();
    let mut info = Vec::new();
    proto::int(1, 6, &mut info);
    proto::bytes(2, &remote.public_key(), &mut info);
    proto::bytes(3, &[8; 16], &mut info);
    proto::fixed(4, 1000, &mut info);
    let tag = session_tag(
        &key[..],
        c.vin.as_bytes(),
        &c.pending.as_ref().unwrap().uuid,
        &info,
    );
    let mut message = reply_envelope(&c);
    proto::bytes(15, &info, &mut message);
    proto::bytes(13, &proto::nested(6, &proto::nested(1, &tag)), &mut message);
    assert!(c.accept_hello(&message).unwrap());
    c
}
fn reply_envelope(c: &Client) -> Vec<u8> {
    let mut out = proto::nested(6, &proto::nested(2, &c.route));
    proto::bytes(7, &[8, 3], &mut out);
    proto::bytes(50, &c.pending.as_ref().unwrap().uuid, &mut out);
    out
}
pub fn synthetic_response(c: &Client, plain: &[u8]) -> Vec<u8> {
    let p = c.pending.as_ref().unwrap();
    let counter = 1u32;
    let flags = 0u32;
    let fault = 0u32;
    let aad = Sha256::digest(metadata(&[
        (0, &[9]),
        (1, &[3]),
        (2, c.vin.as_bytes()),
        (5, &counter.to_be_bytes()),
        (7, &flags.to_be_bytes()),
        (8, &p.request_id),
        (9, &fault.to_be_bytes()),
    ]));
    let (body, tag) = seal(&c.session.as_ref().unwrap().key[..], [9; 12], &aad, plain).unwrap();
    let mut signature = Vec::new();
    proto::bytes(1, &[9; 12], &mut signature);
    proto::int(2, 1, &mut signature);
    proto::bytes(3, &tag, &mut signature);
    let mut out = reply_envelope(c);
    proto::bytes(10, &body, &mut out);
    proto::bytes(13, &proto::nested(9, &signature), &mut out);
    out
}
#[test]
fn encrypted_read_response_is_correlated_authenticated_and_consumed_once() {
    let mut c = synthetic_session();
    let query = c.query(Category::Charge).unwrap();
    assert_eq!(Message::parse(&query).unwrap().int(52).unwrap(), Some(2));
    let mut state = Vec::new();
    proto::int(114, 68, &mut state);
    proto::fixed(111, 200f32.to_bits(), &mut state);
    let body = proto::nested(2, &proto::nested(3, &state));
    let response = synthetic_response(&c, &body);
    let mut tampered = response.clone();
    *tampered.last_mut().unwrap() ^= 1;
    assert_eq!(
        c.accept_response(&tampered, 1000),
        Err(Error::Authentication)
    );
    let mut tampered_fault = response.clone();
    proto::bytes(12, &[8, 2, 16, 3], &mut tampered_fault);
    assert_eq!(
        c.accept_response(&tampered_fault, 1000),
        Err(Error::Authentication),
        "fault in an encrypted message is authenticated before it can stop reads as unauthorized"
    );
    let update = c.accept_response(&response, 1000).unwrap().unwrap();
    assert!(matches!(
        update,
        Update::Charge {
            soc_pct: Some(crate::tesla::Reading { value: 68.0, .. }),
            ..
        }
    ));
    assert_eq!(c.accept_response(&response, 1000).unwrap(), None);
    c.query(Category::Tires).unwrap();
    assert_eq!(
        c.accept_response(&response, 1000).unwrap(),
        None,
        "late UUID cannot complete a new request"
    );
}
#[test]
fn expired_query_and_invalid_hello_never_install_a_session() {
    let mut c = synthetic_session();
    c.query(Category::Charge).unwrap();
    let response = synthetic_response(&c, &proto::nested(2, &proto::nested(3, &[])));
    c.pending.as_mut().unwrap().since = Instant::now() - Duration::from_secs(6);
    assert_eq!(c.accept_response(&response, 1000), Err(Error::Expired));
    let _ = c.hello();
    let mut wrong = reply_envelope(&c);
    proto::bytes(15, &[], &mut wrong);
    assert!(c.accept_hello(&wrong).is_err());
    assert!(c.session.is_none());
}
#[test]
fn wait_unauthorized_and_authenticated_session_resync_follow_official_protocol() {
    let mut c = synthetic_session();
    c.query(Category::Charge).unwrap();
    let mut wait = reply_envelope(&c);
    proto::bytes(12, &[8, 1], &mut wait);
    assert_eq!(c.accept_response(&wait, 1000).unwrap(), None);
    assert!(c.pending.is_some());
    let mut refused = reply_envelope(&c);
    proto::bytes(12, &[8, 2, 16, 3], &mut refused);
    assert_eq!(c.accept_response(&refused, 1000), Err(Error::NotAuthorized));
    let s = c.session.as_ref().unwrap();
    let mut info = Vec::new();
    proto::int(1, 50, &mut info);
    proto::bytes(2, &s.remote_public, &mut info);
    proto::bytes(3, &s.epoch, &mut info);
    proto::fixed(4, 1002, &mut info);
    let tag = session_tag(
        &s.key[..],
        c.vin.as_bytes(),
        &c.pending.as_ref().unwrap().uuid,
        &info,
    );
    let mut update = reply_envelope(&c);
    proto::bytes(15, &info, &mut update);
    proto::bytes(13, &proto::nested(6, &proto::nested(1, &tag)), &mut update);
    assert_eq!(c.accept_response(&update, 1000), Err(Error::Resync));
    assert_eq!(c.session.as_ref().unwrap().counter, 50);
    assert!(c.pending.is_none());
    c.query(Category::Charge).unwrap();
    assert_eq!(c.session.as_ref().unwrap().counter, 51);
    let mut bad = update;
    *bad.last_mut().unwrap() ^= 1;
    assert_eq!(
        c.accept_response(&bad, 1000).unwrap(),
        None,
        "old request UUID does not resync a new query"
    );
}
#[test]
fn enrollment_is_vehicle_monitor_only_and_never_owner_or_driver() {
    let bytes = example_identity().enrollment_request();
    let outer = Message::parse(&bytes).unwrap().nested(1).unwrap().unwrap();
    assert_eq!(outer.int(3).unwrap(), Some(2));
    let payload = Message::parse(outer.bytes(2).unwrap().unwrap()).unwrap();
    let permission = payload
        .nested(16)
        .unwrap()
        .unwrap()
        .nested(5)
        .unwrap()
        .unwrap();
    assert_eq!(permission.int(4).unwrap(), Some(5));
}

#[test]
fn cancelled_query_retires_uuid_without_resetting_session_or_counter() {
    let mut c=synthetic_session();c.query(Category::Charge).unwrap();
    let old=synthetic_response(&c,&proto::nested(2,&proto::nested(3,&[])));
    let counter=c.session.as_ref().unwrap().counter;
    c.cancel_query();c.query(Category::Tires).unwrap();
    assert_eq!(c.session.as_ref().unwrap().counter,counter+1);
    assert_eq!(c.accept_response(&old,1000).unwrap(),None);
    let fresh=synthetic_response(&c,&proto::nested(2,&proto::nested(19,&[])));
    assert!(matches!(c.accept_response(&fresh,1000).unwrap(),Some(Update::Tires(_))));
}

#[test]
fn driver_enrollment_requires_an_explicit_permission() {
    let identity = example_identity();
    assert_eq!(
        identity.enrollment_request(),
        identity.enrollment_request_for(EnrollmentPermission::VehicleMonitor)
    );
    let bytes = identity.enrollment_request_for(EnrollmentPermission::Driver);
    let outer = Message::parse(&bytes).unwrap().nested(1).unwrap().unwrap();
    let permission = Message::parse(outer.bytes(2).unwrap().unwrap())
        .unwrap()
        .nested(16)
        .unwrap()
        .unwrap()
        .nested(5)
        .unwrap()
        .unwrap();
    assert_eq!(permission.int(4).unwrap(), Some(3));
}

#[test]
fn climate_query_and_command_share_authenticated_sessions_without_cross_acceptance() {
    let mut c = synthetic_session();
    let request = c.climate_query().unwrap();
    assert_eq!(Message::parse(&request).unwrap().int(52).unwrap(), Some(2));
    assert_eq!(
        c.climate_command(ClimateCommand::Power { on: true }),
        Err(Error::State)
    );
    let mut fields = Vec::new();
    proto::int(110, 0, &mut fields);
    proto::fixed(103, 21.5f32.to_bits(), &mut fields);
    let mut body = proto::nested(1, &[]);
    body.extend(proto::nested(2, &proto::nested(4, &fields)));
    let response = synthetic_response(&c, &body);
    assert_eq!(c.accept_climate_ack(&response), Err(Error::State));
    let mut tampered = response.clone();
    *tampered.last_mut().unwrap() ^= 1;
    assert_eq!(
        c.accept_climate_response(&tampered),
        Err(Error::Authentication)
    );
    let state = c.accept_climate_response(&response).unwrap().unwrap();
    assert_eq!(state.hvac_on, Some(false));
    assert_eq!(state.driver_temp_celsius, Some(21.5));
    assert_eq!(c.accept_climate_response(&response).unwrap(), None);
    c.climate_command(ClimateCommand::Power { on: true })
        .unwrap();
    assert!(
        !c.accept_climate_ack(&response).unwrap(),
        "late query response cannot acknowledge a command"
    );
    let ack = synthetic_response(&c, &proto::nested(1, &[]));
    assert_eq!(c.accept_response(&ack, 1000), Err(Error::State));
    assert!(c.accept_climate_ack(&ack).unwrap());
    assert!(
        !c.accept_climate_ack(&ack).unwrap(),
        "execution acknowledgement is consumed once"
    );
}

#[test]
fn climate_commands_require_authenticated_application_success_and_retire_on_timeout() {
    let mut c = synthetic_session();
    c.climate_command(ClimateCommand::Power { on: false })
        .unwrap();
    let mut waiting = reply_envelope(&c);
    proto::bytes(12, &[8, 1], &mut waiting);
    assert!(!c.accept_climate_ack(&waiting).unwrap());
    assert!(c.pending.is_some());
    let bare = reply_envelope(&c);
    assert_eq!(c.accept_climate_ack(&bare), Err(Error::Authentication));
    let ack = synthetic_response(&c, &proto::nested(1, &[]));
    let mut tampered = ack.clone();
    *tampered.last_mut().unwrap() ^= 1;
    assert_eq!(c.accept_climate_ack(&tampered), Err(Error::Authentication));
    c.pending.as_mut().unwrap().since = Instant::now() - Duration::from_secs(6);
    assert_eq!(c.accept_climate_ack(&ack), Err(Error::Expired));
    assert!(c.pending.is_none());
    assert!(!c.accept_climate_ack(&ack).unwrap());
    for (body, error) in [
        (proto::nested(1, &[8, 1]), Error::Rejected),
        (Vec::new(), Error::Malformed),
    ] {
        let mut c = synthetic_session();
        c.climate_command(ClimateCommand::Power { on: true })
            .unwrap();
        let response = synthetic_response(&c, &body);
        assert_eq!(c.accept_climate_ack(&response), Err(error));
        assert!(c.pending.is_none());
    }
}

#[test]
fn climate_command_resync_does_not_acknowledge_or_reemit_the_command() {
    let mut c = synthetic_session();
    c.climate_command(ClimateCommand::Power { on: true })
        .unwrap();
    let s = c.session.as_ref().unwrap();
    let mut info = Vec::new();
    proto::int(1, 50, &mut info);
    proto::bytes(2, &s.remote_public, &mut info);
    proto::bytes(3, &s.epoch, &mut info);
    proto::fixed(4, 1002, &mut info);
    let tag = session_tag(
        &s.key[..],
        c.vin.as_bytes(),
        &c.pending.as_ref().unwrap().uuid,
        &info,
    );
    let mut update = reply_envelope(&c);
    proto::bytes(15, &info, &mut update);
    proto::bytes(13, &proto::nested(6, &proto::nested(1, &tag)), &mut update);
    assert_eq!(c.accept_climate_ack(&update), Err(Error::Resync));
    assert!(c.pending.is_none());
    assert_eq!(c.session.as_ref().unwrap().counter, 50);
    assert!(!c.accept_climate_ack(&update).unwrap());
}
