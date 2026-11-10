//! One switch operation coordinates BlueZ, CarPlay admission, and the selected pairing slot.
use crate::bluetooth::Manager;
use f50_server::{Hub, diag::diag};
use std::sync::atomic::Ordering;
use std::{sync::Arc, time::Duration};
/// A newer phone selection (switch, disconnect, new pairing) replaced this operation.
const SUPERSEDED: &str = "此操作已被新的手机选择替代";
fn current(hub: &Hub, epoch: u64) -> crate::RuntimeResult<()> {
    if hub.phones.is_current(epoch) {
        Ok(())
    } else {
        Err(SUPERSEDED.into())
    }
}
pub(crate) async fn wait_current(hub: &Hub, epoch: u64, delay: Duration) -> bool {
    tokio::select! {biased;_=hub.phones.changed_since(epoch)=>false,_=tokio::time::sleep(delay)=>hub.phones.is_current(epoch)}
}
pub(crate) async fn invite_locked(manager: &Manager, hub: &Hub, epoch: u64) -> crate::RuntimeResult<()> {
    current(hub, epoch)?;
    tokio::select! {biased;_=hub.phones.changed_since(epoch)=>Err(SUPERSEDED.into()),r=manager.connect()=>{current(hub,epoch)?;r}}
}
async fn stop_locked(manager: &Manager, hub: &Hub, epoch: u64) -> crate::RuntimeResult<()> {
    current(hub, epoch)?;
    tokio::time::timeout(Duration::from_secs(8), manager.quiesce(epoch))
        .await
        .map_err(|_| "旧手机的蓝牙连接未能及时结束，请重试")??;
    tokio::time::timeout(Duration::from_secs(3), async {
        while hub.has_phone_session() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| "旧手机的 CarPlay 会话未能及时结束，请重试")?;
    current(hub, epoch)
}
pub async fn pair_new(manager: &Arc<Manager>, hub: &Hub) -> crate::RuntimeResult<()> {
    manager.supersede_new_pairing(None);
    let epoch = hub.pause_phone()?;
    let _guard = hub.phones.operation.lock().await;
    stop_locked(manager, hub, epoch).await?;
    manager.open().await?;
    current(hub, epoch)?;
    diag().link_step(
        "bluetooth",
        "active",
        "正在扫描附近设备；在列表选择 iPhone，或从 iPhone 蓝牙选择 cowplay",
    );
    diag().event(
        "info",
        "bt",
        "已结束旧手机连接，打开新手机配对窗口（保留原配对）",
    );
    Ok(())
}
pub async fn pair_device(manager:&Arc<Manager>, hub:&Arc<Hub>, device:&str, generation:u64) -> crate::RuntimeResult<()> {
    let epoch=hub.phones.epoch();
    let _guard=hub.phones.operation.lock().await;
    current(hub,epoch)?;
    let stopped=manager.prepare_pair(device,generation).await?;
    current(hub,epoch)?;
    let manager=manager.clone();
    let hub=hub.clone();
    let device=device.to_owned();
    tokio::spawn(async move {
        match manager.pair(&device,generation,stopped).await {
            // Keep the original epoch: a late Pair reply cannot override a newer selection.
            Ok(()) if hub.phones.is_current(epoch) => connect_paired(&manager,&hub,&device,epoch).await,
            Ok(()) => {}
            Err(error) if hub.phones.is_current(epoch) && manager.status().pairing_error.is_some() => {
                diag().link_fail("bluetooth",error.to_string(),"保持 iPhone 的设置 › 蓝牙页面打开，重新扫描后重试。");
            }
            _=>{}
        }
        // Pair may record the bond and still fail (e.g. a late agent Cancel); never leave the guard set.
        manager.finish_new_pairing(&device);
    });
    Ok(())
}
/// An iPhone that paired from its own Bluetooth settings is invited exactly like an outgoing Pair.
pub async fn connect_new_pairing(manager:&Arc<Manager>, hub:&Arc<Hub>, device:&str, epoch:u64) {
    diag().event("info","bt","新 iPhone 配对完成，正在邀请它连接 CarPlay");
    connect_paired(manager,hub,device,epoch).await;
}
async fn connect_paired(manager:&Arc<Manager>, hub:&Arc<Hub>, device:&str, epoch:u64) {
    // pair_new already released the old phone; preserve the new bond's live ACL.
    let result = async {
        current(hub, epoch)?;
        manager.validate_phone(device).await?;
        let _guard = hub.phones.operation.lock().await;
        current(hub, epoch)?;
        if manager.fresh_pairing().as_deref() != Some(device) { return Err(SUPERSEDED.into()); }
        manager.close_completed_pairing(device, epoch).await?;
        current(hub, epoch)?;
        let candidate = hub.phones.prepare(device)?;
        hub.phones.select(candidate, epoch, || manager.select_phone(device, epoch))?;
        diag().link_step("bluetooth", "active", "配对已保存，正在连接这部 iPhone 的 CarPlay…");
        invite_with_retry(manager, hub, epoch).await
    }.await;
    if let Err(error)=result {
        if hub.phones.is_current(epoch) && !error.to_string().contains(SUPERSEDED) && manager.selected_phone().as_deref()==Some(device) {
            diag().link_fail("bluetooth",format!("配对成功，但自动连接未完成：{error}"),"点“重新连接”，或在 iPhone 列表里选择这台手机。");
        }
    }
    manager.finish_new_pairing(device);
}
pub async fn cancel_pairing(manager:&Manager,hub:&Hub,generation:u64)->crate::RuntimeResult<u64> {
    let observed_epoch=hub.phones.epoch();
    let _guard=hub.phones.operation.lock().await;
    current(hub,observed_epoch)?;
    let status=manager.status();
    // A stale page may cancel only the window it displayed, never a newer window or session.
    let unfinished=matches!(status.phase,"Waiting"|"Confirm"|"Pairing")||manager.fresh_pairing().is_some()||status.pairing_error.is_some();
    if status.generation!=generation||hub.has_phone_session()||!unfinished {
        return Err("配对状态已变化，当前连接未改动；请刷新后重试".into());
    }
    let epoch=hub.pause_phone_if_current(observed_epoch)?;
    manager.supersede_new_pairing(None);
    stop_locked(manager,hub,epoch).await?;
    Ok(epoch)
}
pub async fn disconnect(manager: &Manager, hub: &Hub) -> crate::RuntimeResult<()> {
    manager.supersede_new_pairing(None);
    let epoch = hub.pause_phone()?;
    let _guard = hub.phones.operation.lock().await;
    stop_locked(manager, hub, epoch).await?;
    diag().link_step("bluetooth", "idle", "手机已断开；可从已保存手机中选择连接");
    diag().link_step("carplay", "idle", "手机已断开，配对已保留");
    Ok(())
}
pub async fn reset_adapter(manager: &Manager, hub: &Hub) -> crate::RuntimeResult<()> {
    manager.supersede_new_pairing(None);
    let epoch = hub.pause_phone()?;
    let _guard = hub.phones.operation.lock().await;
    stop_locked(manager, hub, epoch).await?;
    manager.reset_adapter().await?;
    current(hub, epoch)
}
pub async fn connect(
    manager: &Arc<Manager>,
    hub: &Arc<Hub>,
    device: Option<&str>,
) -> crate::RuntimeResult<()> {
    connect_at(manager,hub,device,None).await
}
/// A background request keeps its original selection epoch, even after resolving a preferred phone.
pub(crate) async fn connect_automatically(manager:&Arc<Manager>,hub:&Arc<Hub>,device:Option<&str>,epoch:u64)->crate::RuntimeResult<()> {
    connect_at(manager,hub,device,Some(epoch)).await
}
async fn connect_at(manager:&Arc<Manager>, hub:&Arc<Hub>, device:Option<&str>, expected:Option<u64>) -> crate::RuntimeResult<()> {
    let entry = hub.phones.snapshot();
    if expected.is_some_and(|epoch|epoch!=entry.epoch) {return Err(SUPERSEDED.into())}
    let explicit = expected.is_none() && device.is_some();
    if !explicit && (manager.pairing_open() || !hub.phones.snapshot().admission) {
        return Err("请先完成当前配对，或在手机连接页选择要连接的手机".into());
    }
    let device = device
        .map(str::to_owned)
        .or_else(|| manager.selected_phone())
        .ok_or("还没有保存的手机，请先配对新 iPhone")?;
    manager.validate_phone(&device).await?;
    current(hub, entry.epoch)?;
    manager.supersede_new_pairing(Some(&device));
    let snapshot = hub.phones.snapshot();
    if snapshot.admission
        && manager.selected_phone().as_deref() == Some(&device)
        && hub.phone_verified.load(Ordering::Acquire)
        && snapshot.device.as_deref() == Some(&device)
    {
        return Ok(());
    }
    let epoch = hub.pause_phone_if_current(entry.epoch)?;
    let _guard = hub.phones.operation.lock().await;
    stop_locked(manager, hub, epoch).await?;
    let candidate = hub.phones.prepare(&device)?;
    hub.phones
        .select(candidate, epoch, || manager.select_phone(&device, epoch))?;
    diag().link_step("bluetooth", "active", "正在连接所选 iPhone…");
    let result = invite_with_retry(manager, hub, epoch).await;
    current(hub, epoch)?;
    if let Err(e) = &result {
        let text = e.to_string();
        let title = if text.contains("Timeout") || text.contains("timed out") {
            "所选 iPhone 没有响应蓝牙连接，请确认手机已解锁且蓝牙已开启".to_owned()
        } else {
            format!("连接所选手机未完成：{text}")
        };
        diag().link_fail(
            "bluetooth",
            title,
            "确认手机在附近后重试；也可以选择另一部已保存手机或配对新 iPhone。",
        );
    }
    result
}

async fn invite_with_retry(manager:&Manager, hub:&Hub, epoch:u64) -> crate::RuntimeResult<()> {
    let mut result = invite_locked(manager, hub, epoch).await;
    for _ in 0..2 {
        if !result
            .as_ref()
            .is_err_and(|e| e.to_string().contains("without an RFCOMM channel"))
        {
            break;
        }
        if !wait_current(hub, epoch, Duration::from_millis(2500)).await {
            return current(hub, epoch);
        }
        result = invite_locked(manager, hub, epoch).await;
    }
    result
}
