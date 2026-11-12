// CarPlay /info declaration derived from PlayPort, with truthful minimal touchscreen capabilities.
use crate::{
    bplist::{dict, encode, number, Value},
    Error, Result,
};
// Current receiver envelope; these are not a per-iPhone capability table.
pub const MIN_WIDTH:u16=320; pub const MIN_HEIGHT:u16=240;
// Locked PlayPort AirPlayDisplaySettings declaration range.
pub const FRAME_RATES:[u8;7]=[30,35,40,45,50,55,60];
pub const DISPLAY_UUID: &str = "b7e6c5a0-1111-4000-8000-000000000001";
pub const NAV_HID_UUID: &str = "2a2a2a2b";
pub const MEDIA_HID_UUID: &str = "2a2a2a2c";
// PlayPort AirPlayHid.mediaDescriptor (GPL-3.0), same locked source as navigation.
pub const MEDIA_HID_DESCRIPTOR: &[u8] = &[
    0x05,0x0c,0x09,0x01,0xa1,0x01,0x15,0x00,0x25,0x06,0x05,0x0c,
    0x0a,0x00,0x00,0x0a,0xb0,0x00,0x0a,0xb1,0x00,0x0a,0xcd,0x00,
    0x0a,0xb5,0x00,0x0a,0xb6,0x00,0x0a,0x9e,0x02,0x75,0x08,0x95,0x01,0x81,0x00,0xc0,
];
// PlayPort AirPlayHid.knobDescriptor, locked in research/reused-upstream-lock.json
// (GPL-3.0). Home is Consumer AC Home (0x0223), bit 1 of the four-byte report.
pub const NAV_HID_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, 0x09, 0x08, 0xa1, 0x01,
    0x05, 0x09, 0x09, 0x01, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02,
    0x05, 0x0c, 0x0a, 0x23, 0x02, 0x0a, 0x24, 0x02, 0x95, 0x02, 0x81, 0x02,
    0x95, 0x05, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x01, 0xa1, 0x00,
    0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02, 0xc0,
    0x09, 0x38, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xc0,
];
pub const MAX_WIDTH: u16 = 3840;
pub const MAX_HEIGHT: u16 = 2160;
fn s(v: &str) -> Value {
    Value::String(v.into())
}
fn array(v: Vec<Value>) -> Value {
    Value::Array(v)
}
pub struct DisplayInfo<'a> {
    pub name: &'a str,
    pub model: &'a str,
    pub manufacturer: &'a str,
    pub device_id: &'a str,
    pub bluetooth_mac: &'a str,
    pub width: u16,
    pub height: u16,
    /// Advertised maxFPS (PlayPort: 30–60 in steps of 5).
    pub fps: u8,
    /// Reported panel width in millimetres; iOS keeps controls at a fixed physical size, so a
    /// wider panel yields smaller CarPlay controls (PlayPort `CarPlaySize`).
    pub width_mm: u16,
    /// Offer HEVC (`hevcInfo`); the phone makes the final choice.
    pub hevc: bool,
    /// Calls/Siri through the car browser microphone: declare bidirectional PCM/Opus.
    /// Playback-only mode retains media without offering receiver voice input.
    pub microphone: bool,
    pub right_hand_drive: bool,
}
pub fn build(c: &DisplayInfo) -> Result<Vec<u8>> {
    if !(MIN_WIDTH..=MAX_WIDTH).contains(&c.width)
        || !(MIN_HEIGHT..=MAX_HEIGHT).contains(&c.height)
        || !FRAME_RATES.contains(&c.fps)
        || !(50..=600).contains(&c.width_mm)
    {
        return Err(Error::Malformed);
    }
    let height_mm = ((c.width_mm as u32 * c.height as u32 + c.width as u32 / 2) / c.width as u32).max(1);
    let mut hid = vec![5, 13, 9, 4, 161, 1];
    for _ in 0..2 {
        hid.extend([
            5, 13, 9, 34, 161, 2, 9, 56, 117, 8, 149, 1, 129, 2, 21, 0, 37, 1, 9, 51, 117, 1, 149,
            1, 129, 2, 149, 7, 129, 3, 5, 1, 38,
        ]);
        hid.extend(c.width.to_le_bytes());
        hid.extend([9, 48, 117, 16, 149, 1, 129, 2, 38]);
        hid.extend(c.height.to_le_bytes());
        hid.extend([9, 49, 129, 2, 192]);
    }
    hid.push(192);
    let resource = |id| {
        dict([
            ("resourceID", number(id)),
            ("transferType", number(1)),
            ("transferPriority", number(100)),
            ("takeConstraint", number(100)),
            ("borrowConstraint", number(100)),
            ("unborrowConstraint", number(100)),
        ])
    };
    let area = dict([
        ("widthPixels", number(c.width as u64)),
        ("heightPixels", number(c.height as u64)),
        ("originXPixels", number(0)),
        ("originYPixels", number(0)),
        (
            "safeArea",
            dict([
                ("widthPixels", number(c.width as u64)),
                ("heightPixels", number(c.height as u64)),
                ("originXPixels", number(0)),
                ("originYPixels", number(0)),
                ("drawUIOutsideSafeArea", Value::Boolean(true)),
            ]),
        ),
    ]);
    // Media playback is independent of receiver microphone capture.
    let format = |kind, audio, mask| {
        dict([
            ("type", number(kind)),
            ("audioType", s(audio)),
            ("audioOutputFormats", number(mask)),
        ])
    };
    let pcm = 0x3fc | 0xc000;
    // Mono PCM input and raw Opus packets are implemented by the browser uplink.
    let pcm_mono = 0x154 | 0x4000;
    let with_input = |kind, audio, output, input| {
        let mut entry = format(kind, audio, output);
        if c.microphone { entry.as_dictionary_mut().unwrap().insert("audioInputFormats".into(), number(input)); }
        entry
    };
    let opus = 0x70000000;
    let mut format_list = vec![
        with_input(100, "compatibility", pcm, pcm_mono),
        format(101, "compatibility", pcm),
        with_input(100, "default", pcm | opus, pcm_mono | opus),
        format(100, "alert", pcm | 0x70000000),
        format(100, "media", pcm),
        with_input(100, "telephony", pcm_mono | opus, pcm_mono | opus),
        with_input(100, "speechRecognition", pcm_mono | opus, pcm_mono | opus),
        format(101, "default", pcm | 0x70000000),
        format(102, "media", 0x800000),
    ];
    let voice = |v: &Value| matches!(v.as_dictionary().unwrap().get("audioType").and_then(Value::as_string), Some("telephony" | "speechRecognition"));
    // iPhone rejects a bidirectional voice profile with no input formats, which also
    // prevents media streams from being created. Do not offer voice without a microphone.
    if !c.microphone { format_list.retain(|v| !voice(v)); }
    let formats = array(format_list);
    let latency = |t: u64, audio: Option<&str>| {
        let mut items = vec![("type", number(t)), ("inputLatencyMicros", number(0)), ("outputLatencyMicros", number(0))];
        if let Some(a) = audio {
            items.push(("audioType", s(a)));
        }
        dict(items)
    };
    let mut latency_list = vec![latency(100, None), latency(100, Some("default")),
        latency(100, Some("media")), latency(100, Some("telephony")),
        latency(100, Some("speechRecognition")), latency(100, Some("alert")),
        latency(101, None), latency(101, Some("default")), latency(102, Some("default"))];
    if !c.microphone { latency_list.retain(|v| !voice(v)); }
    let latencies = array(latency_list);
    let mut entries = vec![
        ("sourceVersion", s("366.0")),
        // AudioPCM (bit 18) agrees with our implemented and advertised LPCM formats.
        ("features", number(0x615653aee2 | (1 << 18))),
        ("statusFlags", number(4)),
        ("model", s(c.model)),
        ("manufacturer", s(c.manufacturer)),
        ("deviceID", s(c.device_id)),
        ("bluetoothIDs", array(vec![s(c.bluetooth_mac)])),
        ("name", s(c.name)),
        ("rightHandDrive", Value::Boolean(c.right_hand_drive)),
        ("keepAliveLowPower", Value::Boolean(false)),
        ("keepAliveSendStatsAsBody", Value::Boolean(false)),
        ("audioFormats", formats),
        ("audioLatencies", latencies),
        (
            "extendedFeatures",
            array(vec![s("vocoderInfo"), s("enhancedRequestCarUI")]),
        ),
        (
            "modes",
            dict([
                ("resources", array(vec![resource(1), resource(2)])),
                (
                    "appStates",
                    array(vec![
                        dict([("appStateID", number(2)), ("state", Value::Boolean(false))]),
                        dict([
                            ("appStateID", number(1)),
                            ("speechMode", Value::Integer((-1i64).into())),
                        ]),
                        dict([("appStateID", number(3)), ("state", Value::Boolean(false))]),
                    ]),
                ),
            ]),
        ),
        (
            "displays",
            array(vec![dict([
                ("uuid", s(DISPLAY_UUID)),
                ("type", number(110)),
                ("maxFPS", number(c.fps as u64)),
                ("widthPixels", number(c.width as u64)),
                ("heightPixels", number(c.height as u64)),
                ("widthPhysical", number(c.width_mm as u64)),
                ("heightPhysical", number(height_mm as u64)),
                ("features", number(8 | 2)),
                ("primaryInputDevice", number(0)),
                ("viewAreas", array(vec![area])),
                ("initialViewArea", number(0)),
            ])]),
        ),
        (
            "hidDevices",
            array(vec![dict([
                ("hidProductID", number(1)),
                ("hidVendorID", number(2)),
                ("hidCountryCode", number(0)),
                ("uuid", s("2a2a2a2a")),
                ("name", s("F50 touchscreen")),
                ("displayUUID", s(DISPLAY_UUID)),
                ("hidDescriptor", Value::Data(hid)),
            ]), dict([
                ("hidProductID", number(1)),
                ("hidVendorID", number(2)),
                ("hidCountryCode", number(0)),
                ("uuid", s(NAV_HID_UUID)),
                ("name", s("CowPlay navigation")),
                ("displayUUID", s(DISPLAY_UUID)),
                ("hidDescriptor", Value::Data(NAV_HID_DESCRIPTOR.to_vec())),
            ]), dict([
                ("hidProductID", number(1)), ("hidVendorID", number(2)),
                ("hidCountryCode", number(0)), ("uuid", s(MEDIA_HID_UUID)),
                ("name", s("CowPlay media")), ("displayUUID", s(DISPLAY_UUID)),
                ("hidDescriptor", Value::Data(MEDIA_HID_DESCRIPTOR.to_vec())),
            ])]),
        ),
    ];
    if c.hevc {
        entries.push(("hevcInfo", dict([])));
    }
    // CowPlay settings as an app on the CarPlay home screen; tapping it makes the iPhone send
    // `requestUI`, which the car page answers by opening its settings over the projection.
    entries.push(("oemIconVisible", Value::Boolean(true)));
    entries.push(("oemIconLabel", s(OEM_LABEL)));
    entries.push((
        "oemIcons",
        array(vec![dict([
            ("widthPixels", number(180)),
            ("heightPixels", number(180)),
            ("prerendered", Value::Boolean(true)),
            ("imageData", Value::Data(OEM_ICON.to_vec())),
        ])]),
    ));
    encode(&dict(entries))
}
pub const OEM_LABEL: &str = "CowPlay";
/// 180×180 CowPlay cow/play brand PNG, identical to the web brand resource.
pub const OEM_ICON: &[u8] = include_bytes!("oem-icon.png");

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_media_and_driver_side_match_the_implemented_controls() {
        let mut c=DisplayInfo {name:"test",model:"F50",manufacturer:"test",device_id:"02:00:00:00:00:01",bluetooth_mac:"02:00:00:00:00:01",width:1280,height:720,fps:30,width_mm:180,hevc:false,microphone:false,right_hand_drive:false};
        for right in [false,true] {
            c.right_hand_drive=right;
            let value=crate::bplist::decode(&build(&c).unwrap()).unwrap();let d=value.as_dictionary().unwrap();
            assert_eq!(d["rightHandDrive"].as_boolean(),Some(right));
            let display=d["displays"].as_array().unwrap()[0].as_dictionary().unwrap();
            assert_eq!(display["features"].as_unsigned_integer(),Some(10));
            assert_eq!(display["primaryInputDevice"].as_unsigned_integer(),Some(0));
            let devices=d["hidDevices"].as_array().unwrap();
            let media=devices.iter().map(|v|v.as_dictionary().unwrap()).find(|d|d["uuid"].as_string()==Some(MEDIA_HID_UUID)).unwrap();
            assert_eq!(media["hidDescriptor"].as_data(),Some(MEDIA_HID_DESCRIPTOR));
        }
    }
    #[test]
    fn carplay_home_screen_gets_the_cowplay_settings_app() {
        let c = DisplayInfo { name: "s", model: "F50", manufacturer: "t", device_id: "02:00:00:00:00:01", bluetooth_mac: "02:00:00:00:00:01", width: 1280, height: 720, fps: 30, width_mm: 180, hevc: false, microphone: false, right_hand_drive: false };
        let info = crate::bplist::decode(&build(&c).unwrap()).unwrap();
        let d = info.as_dictionary().unwrap();
        assert_eq!(d["oemIconVisible"].as_boolean(), Some(true));
        assert_eq!(d["oemIconLabel"].as_string(), Some(OEM_LABEL));
        let icon = d["oemIcons"].as_array().unwrap()[0].as_dictionary().unwrap();
        let png = icon["imageData"].as_data().unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 180);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 180);
        assert_eq!(png, include_bytes!("../../../web/src/shared/cowplay-icon.png"), "web and /info must use the same brand image");
        let devices = d["hidDevices"].as_array().unwrap();
        let navigation = devices.iter().filter_map(Value::as_dictionary).find(|d| d["uuid"].as_string() == Some(NAV_HID_UUID)).unwrap();
        assert_eq!(navigation["displayUUID"].as_string(), Some(DISPLAY_UUID));
        assert_eq!(navigation["hidDescriptor"].as_data(), Some(NAV_HID_DESCRIPTOR));
        assert!(d.get("hevcInfo").is_none(), "H.265 is never offered by default");
    }
    #[test]
    fn display_quality_fields_follow_the_requested_spec() {
        let base = |fps, width_mm, hevc| DisplayInfo { name: "s", model: "F50", manufacturer: "t", device_id: "02:00:00:00:00:01", bluetooth_mac: "02:00:00:00:00:01", width: 1920, height: 720, fps, width_mm, hevc, microphone: false, right_hand_drive: false };
        let info = crate::bplist::decode(&build(&base(60, 300, true)).unwrap()).unwrap();
        let d = info.as_dictionary().unwrap();
        let display = d["displays"].as_array().unwrap()[0].as_dictionary().unwrap();
        assert_eq!(display["maxFPS"].as_unsigned_integer(), Some(60));
        assert_eq!(display["widthPhysical"].as_unsigned_integer(), Some(300));
        assert_eq!(display["heightPhysical"].as_unsigned_integer(), Some(113));
        assert!(d.contains_key("hevcInfo"));
        let mut mic = base(30, 180, false);
        mic.microphone = true;
        let info = crate::bplist::decode(&build(&mic).unwrap()).unwrap();
        let formats = info.as_dictionary().unwrap()["audioFormats"].as_array().unwrap().clone();
        let voice: Vec<_> = formats.iter().map(|v| v.as_dictionary().unwrap()).filter(|f| ["telephony", "speechRecognition"].contains(&f["audioType"].as_string().unwrap())).collect();
        assert_eq!(voice.len(), 2);
        for f in voice {
            let input = f["audioInputFormats"].as_unsigned_integer().unwrap();
            for bit in (0..64).map(|b| 1u64 << b).filter(|b| input & b != 0) {
                let parsed = crate::audio::Format::from_bits(bit).unwrap();
                assert!(matches!(parsed.codec, 2 | 3) && parsed.channels == 1,
                    "input must match the implemented mono PCM/Opus uplink");
            }
            assert_eq!(input, f["audioOutputFormats"].as_unsigned_integer().unwrap(),
                "voice input and output share one negotiated format");
        }
        for bad in [base(25, 180, false), base(61, 180, false), base(33, 180, false), base(30, 20, false)] {
            assert!(build(&bad).is_err());
        }
    }
    #[test]
    fn high_resolution_declaration_and_hid_share_the_bounded_dimensions() {
        let mut c=DisplayInfo { name:"synthetic",model:"F50",manufacturer:"test",device_id:"02:00:00:00:00:01",bluetooth_mac:"02:00:00:00:00:01",width:3840,height:2160,fps:45,width_mm:180,hevc:false,microphone:false,right_hand_drive:false };
        let decoded=crate::bplist::decode(&build(&c).unwrap()).unwrap();
        let d=decoded.as_dictionary().unwrap();
        let display=d["displays"].as_array().unwrap()[0].as_dictionary().unwrap();
        assert_eq!(display["widthPixels"].as_unsigned_integer(),Some(3840));
        assert_eq!(display["heightPixels"].as_unsigned_integer(),Some(2160));
        assert_eq!(display["maxFPS"].as_unsigned_integer(),Some(45));
        let hid=d["hidDevices"].as_array().unwrap()[0].as_dictionary().unwrap()["hidDescriptor"].as_data().unwrap();
        assert!(hid.windows(2).any(|v|v==3840u16.to_le_bytes()));
        assert!(hid.windows(2).any(|v|v==2160u16.to_le_bytes()));
        c.width=3842;assert!(build(&c).is_err());
        c.width=3840;c.height=2162;assert!(build(&c).is_err());
    }
    #[test]
    fn supported_pcm_formats_have_a_matching_discovery_feature() {
        let bytes = build(&DisplayInfo {
            name: "synthetic",
            model: "F50",
            manufacturer: "test",
            device_id: "02:00:00:00:00:01",
            bluetooth_mac: "02:00:00:00:00:01",
            width: 1280,
            height: 720,
            fps: 30,
            width_mm: 180,
            hevc: false,
            microphone: false, right_hand_drive: false,
        })
        .unwrap();
        let info = crate::bplist::decode(&bytes).unwrap();
        let d = info.as_dictionary().unwrap();
        let mut pcm_declared = false;
        for entry in d["audioFormats"].as_array().unwrap() {
            let formats = entry.as_dictionary().unwrap()["audioOutputFormats"]
                .as_unsigned_integer()
                .unwrap();
            for bit in 0..64 {
                let value = 1u64 << bit;
                if formats & value != 0 {
                    let supported = crate::audio::Format::from_bits(value)
                        .expect("advertised output format must be implemented");
                    pcm_declared |= supported.codec == 3;
                }
            }
        }
        assert!(pcm_declared, "this receiver supports and advertises PCM");
        let display = d["displays"].as_array().unwrap()[0].as_dictionary().unwrap();
        assert_eq!(display["maxFPS"].as_unsigned_integer(), Some(30));
        assert_eq!(display["heightPhysical"].as_unsigned_integer(), Some(101));
        assert!(!d.contains_key("hevcInfo"));
        assert_ne!(
            d["features"].as_unsigned_integer().unwrap() & (1 << 18),
            0,
            "PCM output formats require the PCM feature in /info and derived discovery TXT"
        );
    }
    #[test]
    fn playback_only_profile_keeps_media_without_bidirectional_voice() {
        let mut c = DisplayInfo {name:"synthetic",model:"F50",manufacturer:"test",device_id:"02:00:00:00:00:01",bluetooth_mac:"02:00:00:00:00:01",width:800,height:480,fps:30,width_mm:180,hevc:false,microphone:true,right_hand_drive:false};
        let on = crate::bplist::decode(&build(&c).unwrap()).unwrap();
        let on = on.as_dictionary().unwrap();
        assert_eq!(on["audioFormats"].as_array().unwrap().len(), 9);
        assert_eq!(on["audioLatencies"].as_array().unwrap().len(), 9);
        let voice = |v:&Value| matches!(v.as_dictionary().unwrap().get("audioType").and_then(Value::as_string), Some("telephony" | "speechRecognition"));
        let expected_formats:Vec<_> = on["audioFormats"].as_array().unwrap().iter().filter(|v| !voice(v)).cloned().map(|mut v| {v.as_dictionary_mut().unwrap().remove("audioInputFormats");v}).collect();
        let expected_latencies:Vec<_> = on["audioLatencies"].as_array().unwrap().iter().filter(|v| !voice(v)).cloned().collect();
        c.microphone = false;
        let off = crate::bplist::decode(&build(&c).unwrap()).unwrap();
        let off = off.as_dictionary().unwrap();
        assert_eq!(off["audioFormats"].as_array().unwrap(), &expected_formats, "no-input mode must retain every playback format without offering bidirectional voice");
        assert_eq!(off["audioLatencies"].as_array().unwrap(), &expected_latencies);
        assert_eq!(expected_formats.len(), 7);
        for (key, value) in on {
            if key != "audioFormats" && key != "audioLatencies" { assert_eq!(&off[key], value, "unrelated declaration changed: {key}"); }
        }
    }
}
