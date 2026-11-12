use std::{fs, process::Command, time::SystemTime};

#[test]
fn configured_probe_reports_selected_adapter_without_taking_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let mut config: serde_json::Value = serde_json::from_str(include_str!("../packaging/config.linux.example.json")).unwrap();
    config["bluetooth"]["adapter"] = serde_json::json!(7);
    config["host"] = serde_json::json!("openwrt");
    let state = dir.path().join("untouched-state");
    config["state_dir"] = serde_json::json!(state);
    let path = dir.path().join("config.json");
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_cowplay")).arg("probe").arg(path).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let output = String::from_utf8(result.stdout).unwrap();
    assert!(output.contains("bluetooth_backend=system\nadapter=hci7\n"));
    assert!(output.contains("host_management=openwrt\n"));
    assert!(!output.contains("mu300_bt_init=") && !output.contains("hci0="));
    assert!(!state.exists());
}

#[test]
fn image_command_rejects_non_elf_payload() {
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("cowplay-image-cli-{}-{nonce}", std::process::id()));
    fs::write(&path, b"Mach-O host executable").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_cowplay"))
        .arg("verify-image")
        .arg(&path)
        .output()
        .unwrap();
    fs::remove_file(path).unwrap();
    assert!(!result.status.success());
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(
        error.contains("requires ELF64 little-endian AArch64 executable"),
        "{error}"
    );
}

#[test]
fn invalid_management_configuration_fails_before_bluetooth_and_identity_setup() {
    let dir = tempfile::tempdir().unwrap();
    let mut config: serde_json::Value = serde_json::from_str(include_str!("../packaging/config.example.json")).unwrap();
    config["bluetooth"] = serde_json::json!({"kind":"system","adapter":7});
    config["state_dir"] = serde_json::json!(dir.path().join("untouched-state"));
    config["management_token_file"] = serde_json::json!(dir.path().join("token"));
    let path = dir.path().join("config.json");
    fs::write(dir.path().join("token"), "invalid-token").unwrap();
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_cowplay")).arg("serve").arg(path).output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("management token"));
    assert!(!dir.path().join("untouched-state").exists());
}
