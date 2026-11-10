pub mod f50;
pub(crate) mod host;
pub mod openwrt;

use std::path::Path;

#[derive(Clone, Default, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Bluetooth {
    #[default]
    F50,
    System {
        #[serde(default)]
        adapter: u16,
    },
}

impl Bluetooth {
    pub fn adapter(&self) -> u16 {
        match self {
            Self::F50 => 0,
            Self::System { adapter } => *adapter,
        }
    }
    pub fn bus(&self) -> &'static str {
        match self {
            Self::F50 => f50::BUS,
            Self::System { .. } => "unix:path=/run/dbus/system_bus_socket",
        }
    }
    pub fn is_f50(&self) -> bool {
        matches!(self, Self::F50)
    }
    pub fn start(
        &self,
        state: &Path,
        address: Option<&Path>,
        stop: tokio::sync::watch::Receiver<bool>,
    ) -> crate::RuntimeResult<Option<f50::Board>> {
        match self {
            Self::F50 => Ok(Some(f50::Board::start(
                state,
                address.ok_or("F50 requires bluetooth_address_file")?,
                stop,
            )?)),
            Self::System { .. } => {
                if !cfg!(target_os = "linux") {
                    return Err("system Bluetooth requires Linux".into());
                }
                if *stop.borrow() {
                    return Err("Bluetooth start cancelled".into());
                }
                f50::private_dir(state)?;
                Ok(None)
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn system_backend_does_not_spawn_or_link_a_private_bluez_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let (stop, rx) = tokio::sync::watch::channel(false);
        assert!(
            Bluetooth::System { adapter: 7 }
                .start(&state, None, rx.clone())
                .unwrap()
                .is_none()
        );
        assert!(state.is_dir());
        assert!(!state.join("bluetooth").exists());
        stop.send(true).unwrap();
        assert!(
            Bluetooth::System { adapter: 7 }
                .start(&dir.path().join("cancelled"), None, rx)
                .is_err()
        );
        assert!(!dir.path().join("cancelled").exists());
    }
}
