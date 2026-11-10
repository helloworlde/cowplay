# Linux／64 位树莓派兼容增量

OpenWrt/F50 为默认平台；普通 Linux 与 64 位树莓派可显式选择系统 BlueZ。下载安装和升级见 [Linux 发布说明](LINUX-RELEASES.md)。

## 平台取舍

| 项目 | 旧配置／F50（默认） | 显式 system 后端 |
| --- | --- | --- |
| 蓝牙初始化 | MU300 厂商初始化及私有 BlueZ/D-Bus，原有子进程清理不变 | 使用主机已有 BlueZ 与 `/run/dbus/system_bus_socket`，不装驱动／固件、不启动／停止主机蓝牙服务 |
| 适配器 | hci0 | `bluetooth.adapter` 数字，例如 1 表示 hci1；对象列表、回调、配对及原始 HCI 使用同一索引 |
| 默认配对 Agent | 私有总线上保留 RequestDefaultAgent | 仅注册本应用 Agent，不抢系统全局默认 Agent；从 CowPlay 扫描页面选择手机开始配对 |
| Wi-Fi 交接信息 | 保留显式／唯一 UCI 热点匹配及密码、信道读取 | 只读 receiver.wifi 配置，不读取或修改主机 UCI／热点配置 |
| 策略 | 默认 `/etc/config/cowplay`，LuCI 继续可用 | 默认 `state_dir/policy.json`；可显式选择独立 UCI 文本文件，无需安装 uci 命令 |
| 本机维护 | 原 `/var/run/cowplay.sock`、rpcd 和 LuCI ACL | 默认 `state_dir/control.sock`，文件权限 0600；CLI 可传 socket 路径 |
| 管理入口 | `/admin` 仍转 LuCI | `/admin` 有独立管理令牌时转 `/manage`，否则转现有手机控制台 |
| Tesla／Ultra | 原默认行为 | 只读 worker 使用所选总线和适配器；授权命令可附服务配置；硬件共存待实测 |

system 后端仍会给**明确选中的控制器**上电、设置 CowPlay Alias／HCI Local Name／EIR，并按配对窗口改变 Pairable／Discoverable。需要为 CowPlay 预留支持 BR/EDR、RFCOMM 的专用适配器；不能把 BLE-only 适配器当作无线 CarPlay 控制器。现有 HCI 名称核验需要 root 或等价原始 HCI 权限，以及系统 D-Bus/BlueZ 策略允许的 Agent/Profile/Adapter 操作。程序不会自动提权。其他适配器的手机与回调不能进入当前会话。

配对文件仍按远端设备 MAC 保存，hci0 的既有文件名与内容不变。每个 CowPlay 实例／本地控制器使用独立 state_dir，不自动移动 BlueZ 系统配对目录，也不把 F50 配对复制到树莓派。无线热点须事先由用户使用主机系统配置好；AP 能力、信道、认证、路由／防火墙和实际 iPhone 交接需现场核验。

## 配置与运行

完整占位示例：`packaging/config.linux.example.json`；保留原 `packaging/config.example.json` 用于 F50。Linux 新增部分如下，其余 receiver 字段仍必须提供合法持有的配件身份、真实控制器地址、已有热点和可验证的 HTTPS 证书：

```json
{
  "state_dir": "/var/lib/cowplay",
  "bluetooth": {"kind": "system", "adapter": 0},
  "policy": {"format": "json", "path": "/var/lib/cowplay/policy.json"},
  "ctl_socket": "/var/lib/cowplay/control.sock",
  "management_token_file": "/var/lib/cowplay/management-token"
}
```

`bluetooth_address_file` 仅 F50 必需；省略 `bluetooth` 永远选 F50，不按运行环境自动切换。`policy` 和旧 `uci_config` 不能同时配置。可用 `{"format":"uci","path":"/var/lib/cowplay/cowplay.uci"}` 保留 UCI 文本格式。策略文件缺失时创建新文件，JSON 权限 0600；JSON 拒绝未知字段、无效枚举、越界画面参数，保存仍复用现有策略语义和原子替换。默认屏幕项沿用 UCI 支持的 quality、call、rail_side；各屏幕完整画面偏好仍在既有屏幕记录中。system 新建策略记录当前配置端口，避免首启／重启地址漂移；已有策略中的 port/domain 是下次启动的地址来源。

可选管理令牌是 **32 个随机字节的 64 位十六进制文本**，与六位投屏令牌分离。按配置示例启用时，须先在目标主机创建该文件，例如由用户本机执行 `umask 077; openssl rand -hex 32 > /var/lib/cowplay/management-token`。本轮未执行这些主机配置命令。文件须由运行服务的用户持有且无组／其他用户权限；读取使用同一 O_NOFOLLOW 文件描述符校验和限长读取。错误配置在任何蓝牙动作前失败。不需要该管理能力时省略 management_token_file。

通用管理页提供服务／连接状态、画面上限及自动降帧、投屏令牌读取、HTTPS 证书成对验证和热切换；手机控制台继续提供手机配对、切换、屏幕偏好、诊断与日志。管理 POST 必须同时满足独立 Bearer 管理令牌和既有 Origin 校验，普通浏览器 cookie 或投屏令牌不授予维护权限，令牌仅保存在当前页面内存，不进 URL、localStorage 或 sessionStorage。HTTP 端口迁移、系统服务控制、恢复出厂未暴露给管理 HTTP；热点、路由、启动、升级由主机工具负责。DDNS 和其他维护仍可使用同一私有控制 socket；system 的 network 迁移命令会明确拒绝 OpenWrt 专用操作。

以下仅为目标主机运行方式，不表示本轮已运行或安装：

```sh
cowplay probe
cowplay serve /etc/cowplay/config.json
cowplay ctl status /var/lib/cowplay/control.sock
cowplay ctl reload /var/lib/cowplay/control.sock
# 可选 Tesla 授权：第三参数使其与服务使用同一 BlueZ 后端与适配器
cowplay tesla-pair /var/lib/cowplay/tesla-ble.json /etc/cowplay/config.json
```

`packaging/cowplay.service` 是可选 systemd 模板，不自动安装或启用，不配置自动重启。它等待 network-online.target 并要求现有 bluetooth.service；主机的 wait-online／AP 配置必须覆盖所选接口与地址，不能把 network-online 本身当作热点已经就绪的证据。模板使用 `/usr/local/bin/cowplay` 与上述配置路径。F50 继续使用原 procd/APK 生命周期。

## 构建与验证

保留 Rust workspace、锁文件和前端工具链，没有新增或升级依赖。Linux 本机运行 `cargo build --release --locked`；64 位 Raspberry Pi OS 应使用 AArch64 Rust 工具链。`scripts/build-rust.sh` 保留默认 `aarch64-unknown-linux-musl`，也可传入已有工具链支持的 Linux target，例如 `x86_64-unknown-linux-gnu`；脚本不安装编译器或下载目标。

自动构建与测试由 GitHub Actions 执行；日志与候选产物以具体 workflow run 为准。构建和隔离测试不代表真实蓝牙、热点、手机或车机验收。

## 剩余验收与恢复

实机验收至少覆盖：F50 原配置启动／有序停止、数字确认、旧配对恢复与双手机切换；树莓派／普通 Linux 的系统 BlueZ 权限及专用适配器、AP 交接、iAP2 认证、CarPlay 会话、真实媒体与浏览器绘制、触控、音频、断线恢复和长期稳定；Tesla 与手机共存另列验收。容器结果不能证明厂商内核或 Raspberry Pi 蓝牙固件行为。

不自动迁移私有状态或删除配对；旧配置仍选择 F50。任何已有用户状态均应完整保留；以后降级或迁移仍遵守项目原有多手机记录、身份和证书规则，不把覆盖旧程序当作无条件安全回退。

平台 API 对照：[BlueZ Adapter API](https://bluez.readthedocs.io/en/latest/adapter-api/)、[Agent API](https://bluez.readthedocs.io/en/latest/agent-api/)、[Profile API](https://bluez.readthedocs.io/en/latest/profile-api/)。系统 Agent 只负责应用主动发起的操作；F50 私有总线继续保留默认 Agent，profile 随连接关闭释放，扫描只释放自身 session。
