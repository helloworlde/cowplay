# CowPlay

Rust 无线 CarPlay 接收端，主要面向安装 MU300 OpenWrt 的中兴 F50，同时提供 Linux x86_64、Linux ARM64／64 位树莓派支持。TypeScript/Vite 网页嵌入静态 Rust 程序。Based on LIVI by Lasse Heitgres (https://github.com/f-io/LIVI).

实现包含 BlueZ Agent1/Profile1、有界手机配对与切换、RFCOMM/iAP2、认证与 Wi-Fi 交接、RTSP/媒体、HTTPS/WSS，以及手机、车机、LuCI 管理页。Tesla BLE 提供可选的只读电量、续航和胎压 Feed，没有车辆控制命令。Ultra、音频、实际手机／车机兼容及长期稳定性仍需实机验收。

项目不提供配件身份、私钥、证书、手机配对或厂商固件。使用者须合法持有配件身份，自行配置可信 HTTPS 证书、蓝牙适配器与无线热点。F50 USB 不具备主机能力，不支持有线 CarPlay。

## 下载与安装

正式附件发布在 [GitHub Releases](https://github.com/helloworlde/cowplay/releases)。目前尚无正式 Release，签名下载安装流程需正式附件发布后使用。

| 系统 | 安装材料 | 说明 |
| --- | --- | --- |
| OpenWrt aarch64_generic | 同版本签名 cowplay、luci-app-cowplay APK | [公钥导入、首次安装、LuCI 更新和系统升级恢复](docs/RELEASES.md) |
| Linux x86_64 | cowplay-版本-linux-x86_64.tar.gz | [签名下载、安装和升级](docs/LINUX.md) |
| Linux ARM64／64 位树莓派 | cowplay-版本-linux-arm64.tar.gz | [签名下载、安装和升级](docs/LINUX.md) |

当前候选包版本为 `0.3.14-r17`。OpenWrt 发布策略仅声明 `25.12.6`、`aarch64_generic`，其他固件必须单独验证。F50 使用厂商 5.4 内核，不能安装官方 6.12 内核模块。OpenWrt 包自带私有 BlueZ/D-Bus 和 musl 用户态依赖，不替换系统驱动、固件或蓝牙服务。

首次安装 OpenWrt 核心包开启下次开机自启，但不立即启动；配置完整后由管理员显式启动。Linux 安装器首次只安装程序和可选 systemd unit，不启用、不启动。升级保留配置、私有状态和原运行／自启选择。签名、公钥指纹核验和兼容性检查不可省略。

## 配置与连接

- F50 配置结构见 `packaging/config.example.json`；Linux 见 `packaging/config.linux.example.json`。示例地址、域名、VIN、身份和密码均为占位值。
- Linux 必须显式设置 `bluetooth.kind=system`，并提供支持 BR/EDR、RFCOMM 的专用适配器及系统 BlueZ/D-Bus。详见 [Linux 兼容配置](docs/LINUX.md)。
- 私有配置、身份、证书和配对保存在设备本地。私有目录使用 `0700`，私钥与配置使用 `0600`；不要上传至本仓库、Issues、Actions artifacts 或 Release。
- 默认投屏访问令牌为 `123456`，部署时按需要修改。Linux 可另设随机管理令牌，与投屏令牌分离。
- F50 的 `/admin` 转至 LuCI；Linux 独立管理页需要单独管理令牌。车机页面为 `/`，手机配对页为 `/pairing`。地址需与可信证书及 Origin 配置匹配。

新手机先在 `/pairing` 打开配对窗口，在 iPhone 蓝牙设置选择 CowPlay，核对两端六码并确认，然后连接 CarPlay。已保存手机可选择连接或切换，一次连接一部手机。取消或断开不删除既有配对。

F50 停服使用 `/etc/init.d/cowplay stop`；`apk del cowplay` 保留私有状态。升级失败后检查事务记录和备份，不直接覆盖旧程序或私有状态进行降级。OpenWrt 保留设置的系统升级可使用签名恢复缓存；`sysupgrade -n` 和任意厂商刷机不保证保留应用，详见发布说明。

Tesla 只读接入使用独立 `tesla-ble.json` 与用户本地生成的身份，没有配置时不扫描、不连接车辆。车主需在车内确认 Vehicle Monitor 授权，密钥和 VIN 留在本地。协议来源见 `research/tesla-ble-lock.json`、`NOTICE.md`。

## 构建与验证

使用锁文件约束 Rust 和 npm 依赖。先构建前端，再编译 Rust，保证嵌入资源与源码一致。

```sh
npm ci --prefix web
npm run build --prefix web
cargo build --release --locked
cargo test --workspace --locked
```

`make help` 列出全部入口：

| 命令 | 用途 |
| --- | --- |
| `make build` | 原生 ARM64 Alpine 容器构建静态程序、私有 BlueZ 运行时与双 APK |
| `make linux-tar LINUX_ARCH=x86_64 LINUX_BINARY=target/release/cowplay` | 打包对应架构静态 ELF；ARM64 使用 LINUX_ARCH=arm64 |
| `make test` | Rust、前端逻辑及包生命周期回归（需相应 APK/OpenSSL 工具） |

GitHub Actions 在 main、PR 和手动触发时使用对应架构原生 runner 构建 OpenWrt、Linux x86_64、Linux ARM64，运行相关测试并上传候选产物。生产发布仅在匹配版本的标签推送后执行，使用专用发布密钥。Environment Secret、公钥指纹和完整流程见 [发布说明](docs/RELEASES.md)。未签名候选产物不能用于正式分发。

```sh
python3 scripts/test-package-lifecycle.py
python3 scripts/test-sysupgrade-restore.py
python3 scripts/test-linux-install.py
python3 scripts/test-linux-download.py
# 原生 ARM64 Linux、APK v3、OpenSSL 环境：
python3 scripts/test-release-artifacts.py
```

测试使用一次性目录与明确合成数据，不操作真实设备。真实蓝牙、热点、iPhone、会话、媒体绘制、触控、音频和断电／固件升级须分别验收。

## 源码与许可

[架构说明](docs/ARCHITECTURE.md) 描述协议、平台、管理和车辆 Feed；[网络端口说明](docs/NETWORK.md) 描述监听和防火墙要求。

项目为 GPL-3.0-only。第三方归属、固定源码版本与运行时许可见 `NOTICE.md`、`research/*lock.json`、`packaging/RUNTIME-NOTICE.md` 及包内许可文件。Release 附件仅包含程序、公开配置示例、安装材料、公钥与签名，不包含开发设备记录或私人资料。
