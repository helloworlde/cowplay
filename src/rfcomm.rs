//! Nonblocking RFCOMM ownership; a session closes its FD before acknowledging release.
use crate::{
    bluetooth::{Channel, Manager},
};
use f50_core::{bootstrap::Bootstrap, mfi::Identity, wireless::Endpoint};
use std::{
    os::fd::{AsRawFd, OwnedFd},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::unix::AsyncFd;
pub fn validate(fd: &OwnedFd) -> crate::RuntimeResult<()> {
    #[cfg(target_os = "linux")]
    {
        let mut domain: libc::c_int = 0;
        let mut len = std::mem::size_of_val(&domain) as libc::socklen_t;
        // SAFETY: domain and len are valid writable buffers; fd is owned and live.
        if unsafe {
            libc::getsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_DOMAIN,
                (&mut domain as *mut libc::c_int).cast(),
                &mut len,
            )
        } != 0
            || domain != libc::AF_BLUETOOTH
        {
            return Err("BlueZ did not supply a Bluetooth socket".into());
        }
        let mut protocol: libc::c_int = 0;
        if unsafe {
            libc::getsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PROTOCOL,
                (&mut protocol as *mut libc::c_int).cast(),
                &mut len,
            )
        } != 0
            || protocol != 3
        {
            return Err("BlueZ did not supply RFCOMM".into());
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = fd;
        return Err("RFCOMM requires Linux".into());
    }
    #[allow(unreachable_code)]
    Ok(())
}
async fn read(fd: &AsyncFd<OwnedFd>, buffer: &mut [u8]) -> std::io::Result<usize> {
    loop {
        let mut ready = fd.readable().await?;
        match ready.try_io(|inner| {
            // SAFETY: buffer has the stated writable length and fd is live.
            let n = unsafe {
                libc::read(
                    inner.get_ref().as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if n < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(n as usize)
            }
        }) {
            Ok(value) => return value,
            Err(_) => continue,
        }
    }
}
async fn write(fd: &AsyncFd<OwnedFd>, bytes: &[u8]) -> std::io::Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        let mut ready = fd.writable().await?;
        match ready.try_io(|inner| {
            // SAFETY: bytes is readable for its length; MSG_NOSIGNAL prevents peer EOF terminating the process.
            #[cfg(target_os = "linux")]
            let n = unsafe {
                libc::send(
                    inner.get_ref().as_raw_fd(),
                    bytes[offset..].as_ptr().cast(),
                    bytes.len() - offset,
                    libc::MSG_NOSIGNAL,
                )
            };
            #[cfg(not(target_os = "linux"))]
            let n = unsafe {
                libc::write(
                    inner.get_ref().as_raw_fd(),
                    bytes[offset..].as_ptr().cast(),
                    bytes.len() - offset,
                )
            };
            if n < 0 {
                Err(std::io::Error::last_os_error())
            } else if n == 0 {
                Err(std::io::ErrorKind::WriteZero.into())
            } else {
                Ok(n as usize)
            }
        }) {
            Ok(n) => offset += n?,
            Err(_) => continue,
        }
    }
    Ok(())
}
pub async fn run(
    channel: Channel,
    manager: Arc<Manager>,
    endpoint: Arc<Endpoint>,
    identity: Arc<dyn Identity>,
    hub: Arc<f50_server::Hub>,
) -> crate::RuntimeResult<()> {
    let epoch = channel.epoch;
    let result = session(channel, endpoint, identity, &hub).await;
    // session() has returned and dropped its only FD before clearing this exact generation.
    manager.complete(epoch);
    result
}
async fn session(
    channel: Channel,
    endpoint: Arc<Endpoint>,
    identity: Arc<dyn Identity>,
    hub: &f50_server::Hub,
) -> crate::RuntimeResult<()> {
    let selection=channel.selection;
    let device=channel.device;
    let mut stop = channel.stop;
    if *stop.borrow() || !hub.phones.is_current(selection) {
        return Ok(());
    }
    let raw = channel.fd.as_raw_fd();
    // SAFETY: fcntl acts on this owned FD; all flags are preserved except nonblocking enablement.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let d = f50_server::diag::diag();
    d.session_begin("rfcomm");
    d.event("ok", "bt", "蓝牙 RFCOMM 通道已连接");
    d.link_step("bluetooth", "active", "蓝牙已连接，iAP2 握手中");
    let result = drive(channel.fd, &mut stop, &endpoint, identity.as_ref(), hub, selection, &device).await;
    if !hub.phones.is_current(selection) {return result;}
    let handed_off = d.link().bluetooth == "ok";
    match &result {
        Ok(()) => {
            d.event("info", "bt", "蓝牙 RFCOMM 通道已关闭");
            if !handed_off && d.link().bluetooth != "fail" {
                d.link_fail("bluetooth", "蓝牙通道在认证完成前关闭", "让 iPhone 靠近 F50 并保持解锁，点“连接 iPhone”重试。");
            }
        }
        Err(e) => {
            d.event("warn", "bt", format!("蓝牙 RFCOMM 通道异常结束：{e}"));
            if !handed_off {
                d.link_fail("bluetooth", format!("蓝牙通道异常：{e}"), "点“连接 iPhone”重试；反复失败可在诊断页重置蓝牙适配器。");
            }
        }
    }
    d.session_end_without_control("蓝牙通道结束，未建立 CarPlay 控制连接");
    result
}
async fn drive(
    fd: std::os::fd::OwnedFd,
    stop: &mut tokio::sync::watch::Receiver<bool>,
    endpoint: &Arc<Endpoint>,
    identity: &dyn Identity,
    hub: &f50_server::Hub,
    selection: u64,
    device: &str,
) -> crate::RuntimeResult<()> {
    let mut phone_changes=hub.phones.subscribe();
    let d = f50_server::diag::diag();
    let fd = AsyncFd::new(fd)?;
    let mut bootstrap = Bootstrap::new(false, 0).map_err(|e| format!("iAP2 start: {e:?}"))?;
    let mut vehicle_updates=hub.vehicle.subscribe();
    let vehicle=hub.vehicle.snapshot();
    bootstrap.set_vehicle(vehicle.enabled,vehicle.values,0).map_err(|e|format!("iAP2 vehicle: {e:?}"))?;
    let mut stage = bootstrap.stage();
    f50_server::diag_echo!(Info, "iap2", "IAP2_STAGE {stage}");
    let started = Instant::now();
    let mut buffer = [0u8; 16384];
    loop {
        let output = bootstrap.output();
        if !output.is_empty() {
            tokio::select! {
                _=stop.changed()=>break,
                r=tokio::time::timeout(Duration::from_secs(3),write(&fd,&output))=>{r??;}
            }
        }
        tokio::select! {
            _=stop.changed()=>break,
            result=vehicle_updates.changed()=>{
                if result.is_err(){break;}
                let vehicle=vehicle_updates.borrow_and_update().clone();
                bootstrap.set_vehicle(vehicle.enabled,vehicle.values,started.elapsed().as_millis() as u64).map_err(|e|format!("iAP2 vehicle: {e:?}"))?;
            },
            _=phone_changes.changed()=>{if !hub.phones.is_current(selection){break}},
            result=read(&fd,&mut buffer)=>{let n=result?;if n==0{break}bootstrap.feed(&buffer[..n],endpoint,identity,started.elapsed().as_millis() as u64).map_err(|e|format!("iAP2: {e:?}"))?;},
            _=tokio::time::sleep(Duration::from_millis(100))=>{bootstrap.advance(started.elapsed().as_millis() as u64).map_err(|e|format!("iAP2 deadline: {e:?}"))?;}
        }
        if bootstrap.stage() != stage {
            stage = bootstrap.stage();
            f50_server::diag_echo!(Info, "iap2", "IAP2_STAGE {stage}");
            match stage {
                "ACCESSORY_IDENTIFYING" => d.link_step("bluetooth", "active", "配件识别中"),
                "ACCESSORY_AUTHENTICATING" => d.link_step("bluetooth", "active", "MFi 配件认证中"),
                "WIFI_HANDOFF_PENDING" => {
                    // The bonded phone just proved itself via iAP2; it will pair-setup over Wi-Fi next.
                    if !hub.phones.handoff(selection,device) {break;}
                    hub.open_pair_setup();
                    d.link_step("bluetooth", "ok", "已认证，已把热点信息交给 iPhone");
                    d.link_step("wifi", "active", "等待 iPhone 加入 F50 热点");
                }
                "ACCESSORY_AUTH_FAILED" => d.link_fail("bluetooth", "iAP2 配件认证失败", "检查自检中的“配件身份”；导出诊断包以便排查。"),
                _ => {}
            }
            if let Some((kind, text)) = iap2_event(stage) {
                d.event(kind, "iap2", text);
            }
        }
    }
    bootstrap.stop();
    Ok(())
}
fn iap2_event(stage: &str) -> Option<(&'static str, &'static str)> {
    Some(match stage {
        "ACCESSORY_IDENTIFYING" => ("info", "iAP2 链路建立，配件识别中"),
        "ACCESSORY_AUTHENTICATING" => ("info", "iAP2 配件认证中"),
        "WIFI_HANDOFF_PENDING" => ("ok", "iAP2 鉴权通过，下发 Wi‑Fi 凭证"),
        "ACCESSORY_AUTH_FAILED" => ("error", "iAP2 配件认证失败"),
        _ => return None,
    })
}
