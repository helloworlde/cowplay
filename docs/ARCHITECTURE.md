# CowPlay 当前模块边界

本文描述协议、应用、平台与管理层边界。OpenWrt/F50 为默认后端；普通 Linux 支持显式系统 BlueZ，见 [兼容说明](LINUX-COMPAT-2026-11-10.md)。

## 协议、车辆数据与服务

| 位置 | 职责 |
| --- | --- |
| `crates/core/src/` | 无平台 I/O 的 CarPlay、iAP2、认证和媒体协议组件 |
| `crates/core/src/vehicle.rs` | 车辆读数、更新、有效值与现有时效规则；不依赖 Tesla 解码或 Ultra |
| `crates/core/src/tesla/` | Tesla 查询、协议编解码与认证会话；把 Tesla 数据转成通用车辆更新 |
| `crates/server/src/vehicle.rs` | 共享车辆 Feed、连接状态、分类失效与重试通知；由数据生产者写入，iAP2/Ultra 订阅读取 |
| `crates/server/src/ultra/` | Ultra 声明及原生车辆状态传输，消费共享 Feed |
| `crates/server/src/receiver.rs`、`lib.rs` | CarPlay 接收会话与浏览器媒体桥接 |
| `src/tesla_ble/` | Tesla BLE 传输、轮询和任务生命周期，连接 Tesla 协议与共享 Feed |

依赖方向是：Tesla 数据生产者 → 通用车辆模型／Feed ← iAP2 和 Ultra 消费者。通用车辆状态不再从 Tesla 模块导入类型，共享 Feed 不再归 Ultra 所有。Tesla 的查询编码仍留在 Tesla 模块。

## 应用与平台

| 位置 | 职责 |
| --- | --- |
| `src/service.rs` | 组合配置、可选择的蓝牙生命周期、接收器、Web 和管理任务 |
| `src/bluetooth.rs`、`src/bluetooth/controller_name.rs` | BlueZ Agent/Profile、手机蓝牙操作，以及控制器名称/EIR 的读回与修正 |
| `src/phone_flow.rs`、`src/rfcomm.rs` | 手机选择、连接取消和 RFCOMM/iAP2 驱动 |
| `src/platform/mod.rs` | 显式 F50／系统 BlueZ 选择；默认 F50，系统后端不启动或停止主机 BlueZ |
| `src/policy.rs` | UCI／JSON 策略文件，复用现有策略验证和原子保存 |
| `src/platform/f50.rs` | MU300 初始化、私有 BlueZ/D-Bus 子进程、占用与存储链接管理 |
| `src/platform/openwrt/` | UCI 解析与持久化、OpenWrt/uhttpd 端口迁移任务 |
| `src/platform/host.rs` | 现有主机 `/proc`、thermal、路由、地址、进程和 `iw` 采样原语 |

应用通用错误类型由根模块 `RuntimeResult` 定义，蓝牙、会话与 Web 不再仅为错误类型依赖 F50 模块。F50 平台保留原初始化与清理实现；系统后端使用已有系统 D-Bus 和显式适配器，不自动探测或接管其他系统服务。

## 管理层与页面

`src/admin.rs` 保留共享状态、策略应用、手机重连和日志配置；各项实现按职责拆分：

- `admin/preferences.rs`：偏好类型、旧配置转换、画面尺寸校验和日志目录约束。
- `admin/display.rs`：画面参数协商、自动降帧和生效顺序。
- `admin/certificates.rs`：DDNS、DNS-01 任务、证书切换及续期。
- `admin/diagnostics.rs`：定时采样、概览、系统与进程报告、自检、持久日志状态及有界历史导出、脱敏诊断包。
- `admin/management.rs`：可选的独立管理令牌鉴权，复用本机维护实现；令牌与普通投屏 cookie 分离，POST 要求同源。
- `admin/http.rs`：管理层提供给车机/手机的 HTTP 路由、鉴权与响应。

`src/console.rs` 继续提供车机/手机页面的状态、命令和偏好接口，`src/ctl.rs` 提供本机维护入口；LuCI 保留其登录和 ACL 边界。职责搬移本身未改前端源码、嵌入资源或路由；后续 r16 整合保留其现有行为。

## 持久日志

`src/persistent_logs.rs` 负责持久存储挂载核验、日志文件安全打开与有界历史读取；`admin/preferences.rs` 调用其挂载核验，`admin.rs` 配置日志，`admin/diagnostics.rs` 与 `admin/http.rs` 提供状态、自检及经过鉴权的导出。`crates/server/src/diag.rs` 负责诊断记录、脱敏和持久写入队列，`log_store.rs` 负责文件写入、轮转、保留及同步。`src/service.rs` 在原有总退出期限内等待诊断写入排空。

## 兼容与后续边界

旧公开入口 `core::tesla::{Category, Reading, Telemetry, Update, Values}`、`server::ultra::feed`、`cowplay::{board, network, uci}` 和 Admin 的公开类型/函数通过重导出保留。职责拆分本身未改变配置、配对文件、JSON 字段、TLS、BUILD_ID、PROTOCOL 或 APK 版本；版本与兼容范围由发布清单声明。

Linux 兼容增量提供 `bluetooth.kind=system` 和非 `hci0` 适配器，默认使用状态目录中的 JSON 策略。F50 旧配置仍使用厂商初始化、私有 BlueZ、hci0、UCI 与 LuCI。系统后端不请求全局 default agent；配对从 CowPlay 页面主动发起。Tesla 服务与可选带服务配置的授权命令使用对应总线／适配器。通用车辆模型、Ultra 默认声明和 F50/Tesla 优先适配行为保持不变。普通 Linux／树莓派实机完整链路仍待验收。
