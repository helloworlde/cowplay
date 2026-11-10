//! Verify the controller rather than treating BlueZ's cached Alias as a hardware readback.
use crate::RuntimeResult as Result;
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd;
use std::{
    io,
    os::fd::{AsRawFd, OwnedFd},
    time::Duration,
};
use tokio::{io::unix::AsyncFd, sync::Mutex};

const READ_NAME: u16 = 0x0c14;
const WRITE_NAME: u16 = 0x0c13;
const READ_EIR: u16 = 0x0c51;
const WRITE_EIR: u16 = 0x0c52;
const BUDGET: Duration = Duration::from_secs(2);

#[derive(Debug, serde::Serialize)]
pub(crate) struct Names {
    pub controller_name: String,
    pub eir_name: Option<String>,
}

#[derive(Default)]
pub(crate) struct Controller {
    device: u16,
    // Inspection and correction share a gate. Cancellation closes this operation's socket
    // before unlocking, discarding replies already queued to it. It cannot roll back a command
    // already submitted to the kernel/controller.
    gate: Mutex<()>,
    #[cfg(test)]
    mock: Option<std::sync::Arc<test_support::Mock>>,
}

impl Controller {
    pub(crate) fn new(device: u16) -> Self { Self { device, ..Self::default() } }
    fn channel(&self) -> Result<Channel> {
        #[cfg(test)]
        if let Some(mock) = &self.mock {
            return Ok(Channel::Mock(mock.clone()));
        }
        Ok(Channel::Raw(Raw::open(self.device)?))
    }

    pub(crate) async fn inspect(&self) -> Result<Names> {
        tokio::time::timeout(BUDGET, async {
            let _guard = self.gate.lock().await;
            let channel = self.channel()?;
            let name = read_name(&channel).await?;
            let (_, eir) = read_eir(&channel).await?;
            Ok(Names {
                controller_name: name,
                eir_name: complete_name(&eir)?,
            })
        })
        .await
        .map_err(|_| "读取控制器名称超时")?
    }

    pub(crate) async fn synchronize(&self, target: &str) -> Result<Names> {
        if target.is_empty() || target.len() > 238 || target.as_bytes().contains(&0) {
            return Err("蓝牙名称无法同时用于 Local Name 和 EIR".into());
        }
        tokio::time::timeout(BUDGET, async {
            let _guard = self.gate.lock().await;
            let channel = self.channel()?;
            if read_name(&channel).await? != target {
                let mut parameter = [0; 248];
                parameter[..target.len()].copy_from_slice(target.as_bytes());
                channel.command(WRITE_NAME, &parameter).await?;
                if read_name(&channel).await? != target {
                    return Err("控制器蓝牙名称未生效".into());
                }
            }
            let (fec, original) = read_eir(&channel).await?;
            let desired = named_eir(&original, target)?;
            if original != desired {
                let mut parameter = Vec::with_capacity(241);
                parameter.push(fec);
                parameter.extend_from_slice(&desired);
                channel.command(WRITE_EIR, &parameter).await?;
                let (actual_fec, actual) = read_eir(&channel).await?;
                // BlueZ may add service UUIDs concurrently. Verify every name field without
                // requiring those unrelated fields to remain byte-for-byte frozen.
                if actual_fec != fec || !has_target_name(&actual, target)? {
                    return Err("控制器 EIR 蓝牙名称未生效".into());
                }
            }
            Ok(Names {
                controller_name: target.to_owned(),
                eir_name: Some(target.to_owned()),
            })
        })
        .await
        .map_err(|_| "同步控制器蓝牙名称超时")?
    }

    #[cfg(test)]
    pub(crate) fn mocked() -> Self {
        Self {
            device: 0,
            gate: Mutex::new(()),
            mock: Some(Default::default()),
        }
    }
    #[cfg(test)]
    pub(crate) fn mock(&self) -> std::sync::Arc<test_support::Mock> {
        self.mock.as_ref().unwrap().clone()
    }
}

enum Channel {
    Raw(Raw),
    #[cfg(test)]
    Mock(std::sync::Arc<test_support::Mock>),
}
impl Channel {
    async fn command(&self, opcode: u16, parameters: &[u8]) -> Result<Vec<u8>> {
        match self {
            Self::Raw(socket) => socket.command(opcode, parameters).await,
            #[cfg(test)]
            Self::Mock(mock) => mock.command(opcode, parameters).await,
        }
    }
}

async fn read_name(channel: &Channel) -> Result<String> {
    let reply = channel.command(READ_NAME, &[]).await?;
    if reply.len() != 248 {
        return Err("控制器 Local Name 响应长度错误".into());
    }
    let end = reply.iter().position(|b| *b == 0).unwrap_or(reply.len());
    Ok(std::str::from_utf8(&reply[..end])
        .map_err(|_| "控制器名称不是有效 UTF-8")?
        .to_owned())
}
async fn read_eir(channel: &Channel) -> Result<(u8, [u8; 240])> {
    let reply = channel.command(READ_EIR, &[]).await?;
    if reply.len() != 241 {
        return Err("控制器 EIR 响应长度错误".into());
    }
    Ok((reply[0], reply[1..].try_into()?))
}

fn fields(eir: &[u8; 240]) -> Result<Vec<&[u8]>> {
    let mut result = Vec::new();
    let mut offset = 0;
    while offset < eir.len() {
        let size = eir[offset] as usize;
        if size == 0 {
            if eir[offset..].iter().any(|v| *v != 0) {
                return Err("EIR 结束标记后有未知数据，未覆盖原内容".into());
            }
            break;
        }
        let end = offset + size + 1;
        if end > eir.len() {
            return Err("EIR 字段长度错误，未覆盖原内容".into());
        }
        result.push(&eir[offset..end]);
        offset = end;
    }
    Ok(result)
}
fn complete_name(eir: &[u8; 240]) -> Result<Option<String>> {
    let mut name = None;
    for field in fields(eir)? {
        if field[1] == 9 {
            let value = std::str::from_utf8(&field[2..]).map_err(|_| "EIR 名称不是有效 UTF-8")?;
            if name.as_deref().is_some_and(|old| old != value) {
                return Err("EIR 包含不一致的完整名称".into());
            }
            name = Some(value.to_owned());
        }
    }
    Ok(name)
}
fn has_target_name(eir: &[u8; 240], target: &str) -> Result<bool> {
    let fields = fields(eir)?;
    Ok(fields
        .iter()
        .any(|f| f[1] == 9 && &f[2..] == target.as_bytes())
        && fields
            .iter()
            .filter(|f| matches!(f[1], 8 | 9))
            .all(|f| &f[2..] == target.as_bytes()))
}
fn named_eir(original: &[u8; 240], target: &str) -> Result<[u8; 240]> {
    let mut output = Vec::new();
    let mut inserted = false;
    for field in fields(original)? {
        if matches!(field[1], 8 | 9) {
            if inserted {
                continue;
            }
            output.extend([target.len() as u8 + 1, 9]);
            output.extend_from_slice(target.as_bytes());
            inserted = true;
        } else {
            output.extend_from_slice(field);
        }
    }
    if !inserted {
        output.extend([target.len() as u8 + 1, 9]);
        output.extend_from_slice(target.as_bytes());
    }
    if output.len() > 240 {
        return Err("EIR 没有名称空间，未删除已有服务字段".into());
    }
    output.resize(240, 0);
    Ok(output.try_into().unwrap())
}

struct Raw {
    fd: AsyncFd<OwnedFd>,
    filter: bool,
}
impl Raw {
    #[cfg(target_os = "linux")]
    fn open(device: u16) -> Result<Self> {
        #[repr(C)]
        struct Address {
            family: libc::sa_family_t,
            device: u16,
            channel: u16,
        }
        // AF_BLUETOOTH / BTPROTO_HCI, HCI_CHANNEL_RAW, matching BlueZ's hci_open_dev.
        let raw = unsafe {
            libc::socket(
                libc::AF_BLUETOOTH,
                libc::SOCK_RAW | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                1,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: socket returned a new owned descriptor.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let address = Address {
            family: libc::AF_BLUETOOTH as _,
            device,
            channel: 0,
        };
        // SAFETY: address is valid for its exact size and fd remains owned here.
        if unsafe {
            libc::bind(
                fd.as_raw_fd(),
                (&address as *const Address).cast(),
                std::mem::size_of::<Address>() as _,
            )
        } < 0
        {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Self {
            fd: AsyncFd::new(fd)?,
            filter: true,
        })
    }
    #[cfg(not(target_os = "linux"))]
    fn open(_device: u16) -> Result<Self> {
        Err("控制器 HCI 名称读回需要 Linux".into())
    }

    fn set_filter(&self, opcode: u16) -> Result<()> {
        if !self.filter {
            return Ok(());
        }
        #[cfg(not(target_os = "linux"))]
        let _ = opcode;
        #[cfg(target_os = "linux")]
        {
            #[repr(C)]
            struct Filter {
                packet: u32,
                events: [u32; 2],
                opcode: u16,
            }
            // Zero padding as well; accept only this command's Complete/Status events.
            let mut filter: Filter = unsafe { std::mem::zeroed() };
            filter.packet = 1 << 4; // HCI_EVENT_PKT
            filter.events[0] = (1 << 14) | (1 << 15);
            filter.opcode = opcode.to_le();
            // SOL_HCI=0, HCI_FILTER=2, from Linux's bluetooth/hci.h.
            if unsafe {
                libc::setsockopt(
                    self.fd.get_ref().as_raw_fd(),
                    0,
                    2,
                    (&filter as *const Filter).cast(),
                    std::mem::size_of::<Filter>() as _,
                )
            } < 0
            {
                return Err(io::Error::last_os_error().into());
            }
        }
        Ok(())
    }

    async fn command(&self, opcode: u16, parameters: &[u8]) -> Result<Vec<u8>> {
        if parameters.len() > 255 {
            return Err("HCI 参数长度错误".into());
        }
        self.set_filter(opcode)?;
        let mut packet = vec![1, opcode as u8, (opcode >> 8) as u8, parameters.len() as u8];
        packet.extend_from_slice(parameters);
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let mut ready = self.fd.writable().await?;
                match ready.try_io(|fd| {
                    // SAFETY: packet is readable for its length, and fd is live.
                    let n = unsafe {
                        libc::send(
                            fd.get_ref().as_raw_fd(),
                            packet.as_ptr().cast(),
                            packet.len(),
                            0,
                        )
                    };
                    if n < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(n as usize)
                    }
                }) {
                    Ok(result) => {
                        if result? != packet.len() {
                            return Err("HCI 命令未完整发送".into());
                        }
                        break;
                    }
                    Err(_) => continue,
                }
            }
            let mut buffer = [0; 260];
            loop {
                let mut ready = self.fd.readable().await?;
                let size = match ready.try_io(|fd| {
                    // SAFETY: buffer is writable for its full length, and fd is live.
                    let n = unsafe {
                        libc::recv(
                            fd.get_ref().as_raw_fd(),
                            buffer.as_mut_ptr().cast(),
                            buffer.len(),
                            0,
                        )
                    };
                    if n < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(n as usize)
                    }
                }) {
                    Ok(size) => size?,
                    Err(_) => continue,
                };
                if size == 0 {
                    return Err("HCI 控制器连接已关闭".into());
                }
                if let Some(reply) = completion(opcode, &buffer[..size])? {
                    return Ok(reply);
                }
            }
        })
        .await
        .map_err(|_| "HCI 名称命令超时")?
    }
}

fn completion(opcode: u16, packet: &[u8]) -> Result<Option<Vec<u8>>> {
    if packet.first() != Some(&4) {
        return Ok(None);
    }
    if packet.len() < 3 || packet[2] as usize + 3 != packet.len() {
        return Err("HCI 事件长度错误".into());
    }
    let (actual, status, payload) = match packet[1] {
        14 if packet.len() >= 7 => (
            u16::from_le_bytes([packet[4], packet[5]]),
            packet[6],
            Some(&packet[7..]),
        ),
        15 if packet.len() == 7 => (u16::from_le_bytes([packet[5], packet[6]]), packet[3], None),
        14 | 15 => return Err("HCI 完成事件不完整".into()),
        _ => return Ok(None),
    };
    if actual != opcode {
        return Ok(None);
    }
    if status != 0 {
        return Err(format!("HCI 名称命令 0x{opcode:04x} 返回状态 0x{status:02x}").into());
    }
    Ok(payload.map(<[u8]>::to_vec))
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::{
        Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    };
    pub(crate) struct State {
        pub name: String,
        pub eir: [u8; 240],
        pub commands: Vec<u16>,
        pub reject: Option<u16>,
        pub ignore_name_write: bool,
        pub ignore_eir_write: bool,
    }
    pub(crate) struct Mock {
        pub state: StdMutex<State>,
        pub hold_write: AtomicBool,
        pub entered: tokio::sync::Notify,
    }
    impl Default for Mock {
        fn default() -> Self {
            Self {
                state: StdMutex::new(State {
                    name: "cowplay".into(),
                    eir: named_eir(&[0; 240], "cowplay").unwrap(),
                    commands: vec![],
                    reject: None,
                    ignore_name_write: false,
                    ignore_eir_write: false,
                }),
                hold_write: AtomicBool::new(false),
                entered: Default::default(),
            }
        }
    }
    impl Mock {
        pub(super) async fn command(&self, opcode: u16, parameters: &[u8]) -> Result<Vec<u8>> {
            if opcode == WRITE_NAME && self.hold_write.load(Ordering::SeqCst) {
                self.entered.notify_one();
                std::future::pending::<()>().await;
            }
            let mut state = self.state.lock().unwrap();
            state.commands.push(opcode);
            if state.reject == Some(opcode) {
                return Err("synthetic controller error status 0x0c".into());
            }
            match opcode {
                READ_NAME => {
                    let mut data = vec![0; 248];
                    data[..state.name.len()].copy_from_slice(state.name.as_bytes());
                    Ok(data)
                }
                WRITE_NAME => {
                    assert_eq!(parameters.len(), 248);
                    if !state.ignore_name_write {
                        state.name = String::from_utf8(
                            parameters.iter().copied().take_while(|b| *b != 0).collect(),
                        )
                        .unwrap();
                    }
                    Ok(vec![])
                }
                READ_EIR => {
                    let mut data = vec![0];
                    data.extend_from_slice(&state.eir);
                    Ok(data)
                }
                WRITE_EIR => {
                    assert_eq!(parameters.len(), 241);
                    if !state.ignore_eir_write {
                        state.eir.copy_from_slice(&parameters[1..]);
                    }
                    Ok(vec![])
                }
                _ => panic!("unexpected HCI command"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn repairs_vendor_name_and_empty_eir_even_when_software_alias_is_correct() {
        let controller = Controller::mocked();
        let mock = controller.mock();
        {
            let mut s = mock.state.lock().unwrap();
            s.name = "SPREADTRUM BT".into();
            s.eir = [0; 240];
        }
        controller.synchronize("cowplay").await.unwrap();
        let actual = controller.inspect().await.unwrap();
        assert_eq!(actual.controller_name, "cowplay");
        assert_eq!(actual.eir_name.as_deref(), Some("cowplay"));
        assert!(mock.state.lock().unwrap().commands.contains(&WRITE_NAME));
    }
    #[test]
    fn eir_replaces_names_and_preserves_uuid_device_id_and_manufacturer_fields() {
        let blocks = [
            vec![3, 3, 0xaa, 0xfe],
            vec![4, 8, b'o', b'l', b'd'],
            vec![5, 0x10, 1, 2, 3, 4],
            vec![3, 0xff, 7, 8],
            vec![2, 9, b'x'],
        ];
        let mut eir = [0; 240];
        let raw = blocks.concat();
        eir[..raw.len()].copy_from_slice(&raw);
        let changed = named_eir(&eir, "cowplay").unwrap();
        let data = fields(&changed).unwrap();
        assert_eq!(data.iter().filter(|v| matches!(v[1], 8 | 9)).count(), 1);
        assert_eq!(complete_name(&changed).unwrap().as_deref(), Some("cowplay"));
        assert_eq!(
            data.iter()
                .filter(|v| !matches!(v[1], 8 | 9))
                .copied()
                .collect::<Vec<_>>(),
            vec![
                blocks[0].as_slice(),
                blocks[2].as_slice(),
                blocks[3].as_slice()
            ]
        );
        assert_eq!(named_eir(&changed, "cowplay").unwrap(), changed);
    }
    #[test]
    fn full_or_malformed_eir_is_not_silently_cleared() {
        let mut full = [0x11; 240];
        full[0] = 239;
        full[1] = 0xff;
        assert!(named_eir(&full, "cowplay").is_err());
        let mut bad = [0; 240];
        bad[239] = 2;
        assert!(named_eir(&bad, "cowplay").is_err());
    }
    #[tokio::test]
    async fn a_successful_write_without_matching_readback_is_an_error() {
        let controller = Controller::mocked();
        let mock = controller.mock();
        {
            let mut s = mock.state.lock().unwrap();
            s.name = "SPREADTRUM BT".into();
            s.ignore_name_write = true;
        }
        assert!(controller.synchronize("cowplay").await.is_err());
        assert!(!mock.state.lock().unwrap().commands.contains(&WRITE_EIR));
    }
    #[tokio::test]
    async fn an_acknowledged_eir_write_without_the_name_is_an_error() {
        let controller = Controller::mocked();
        let mock = controller.mock();
        {
            let mut s = mock.state.lock().unwrap();
            s.eir = [0; 240];
            s.ignore_eir_write = true;
        }
        assert!(controller.synchronize("cowplay").await.is_err());
    }
    #[tokio::test]
    async fn cancellation_before_send_releases_the_gate_without_writing() {
        use std::sync::{Arc, atomic::Ordering};
        let controller = Arc::new(Controller::mocked());
        let mock = controller.mock();
        mock.state.lock().unwrap().name = "SPREADTRUM BT".into();
        mock.hold_write.store(true, Ordering::SeqCst);
        let task = tokio::spawn({
            let c = controller.clone();
            async move { c.synchronize("cowplay").await }
        });
        mock.entered.notified().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let read = tokio::time::timeout(Duration::from_millis(100), controller.inspect())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read.controller_name, "SPREADTRUM BT");
        assert!(!mock.state.lock().unwrap().commands.contains(&WRITE_NAME));
    }
    #[test]
    fn command_completion_checks_status_length_and_opcode() {
        assert!(completion(WRITE_NAME, &[4, 14, 4, 1, 0x13, 0x0c, 0x0c]).is_err());
        assert!(completion(WRITE_NAME, &[4, 15, 4, 0x0c, 1, 0x13, 0x0c]).is_err());
        assert!(completion(WRITE_NAME, &[4, 14, 5, 1, 0x13, 0x0c, 0]).is_err());
        assert_eq!(
            completion(WRITE_NAME, &[4, 14, 4, 1, 0x14, 0x0c, 0]).unwrap(),
            None
        );
        assert_eq!(
            completion(WRITE_NAME, &[4, 15, 4, 0, 1, 0x13, 0x0c]).unwrap(),
            None
        );
        assert_eq!(
            completion(WRITE_NAME, &[4, 14, 4, 1, 0x13, 0x0c, 0]).unwrap(),
            Some(vec![])
        );
    }
    #[tokio::test]
    async fn actual_socket_request_and_error_reply_use_hci_packet_boundaries() {
        let (client, server) = std::os::unix::net::UnixDatagram::pair().unwrap();
        client.set_nonblocking(true).unwrap();
        server.set_nonblocking(true).unwrap();
        let raw = Raw {
            fd: AsyncFd::new(client.into()).unwrap(),
            filter: false,
        };
        let server = tokio::net::UnixDatagram::from_std(server).unwrap();
        let peer = tokio::spawn(async move {
            let mut data = [0; 260];
            let n = server.recv(&mut data).await.unwrap();
            assert_eq!(&data[..n], &[1, 0x14, 0x0c, 0]);
            server.send(&[4, 14, 4, 1, 0x14, 0x0c, 0x0c]).await.unwrap();
        });
        assert!(raw.command(READ_NAME, &[]).await.is_err());
        peer.await.unwrap();
    }
    #[tokio::test]
    async fn aborting_an_actual_request_closes_its_owned_socket() {
        let (client, server) = std::os::unix::net::UnixDatagram::pair().unwrap();
        client.set_nonblocking(true).unwrap();
        server.set_nonblocking(true).unwrap();
        let raw = Raw {
            fd: AsyncFd::new(client.into()).unwrap(),
            filter: false,
        };
        let server = tokio::net::UnixDatagram::from_std(server).unwrap();
        let task = tokio::spawn(async move { raw.command(READ_NAME, &[]).await });
        let mut request = [0; 4];
        assert_eq!(server.recv(&mut request).await.unwrap(), 4);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(server.send(&[4, 14, 4, 1, 0x14, 0x0c, 0]).await.is_err());
    }
}
