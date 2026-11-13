# 项目状态

CowPlay 当前处于开发预发布阶段。`Pre-release` 面向愿意测试和反馈的使用者，不能等同正式稳定版。

| 范围 | 当前边界 |
| --- | --- |
| F50 / MU300 OpenWrt | 主要适配目标；无线会话、网页显示和输入已有实现与现场记录，当前组合版本仍需回归 |
| Linux x86_64 / ARM64 | 提供构建、安装与系统蓝牙后端；完整无线 CarPlay 链路未完成验收 |
| 手机管理 | 保存独立配对并提供连接／切换；真实双手机切换及中断恢复仍需验收 |
| 音频、通话与麦克风 | 协议和浏览器链路已有实现，实际音频与车机路由仍需验收 |
| Ultra / Tesla BLE | 开发中的可选能力，真实车辆授权、控制及共存效果未完成验收 |
| 长期运行与固件升级 | 本地及隔离回归不能替代长时间、断电、热点中断或真实固件升级测试 |

正式版须完成对应平台构建、安装与安全检查，以及明确范围的真实设备验收，再由维护者确认发布；开发构建不能自动成为正式版。

## 2026-11-13 Tesla 页面与配对修复

开发预发布 v0.0.2-r7；使用合成 VIN、车辆回复与媒体，不播放声音。

| 要求／问题 | 处理结果 |
| --- | --- |
| LuCI Tesla 与其他页面风格一致 | 复用共用页头、导航、主题和操作区；固定官方 LuCI 运行时 `ddccfc9`，Bootstrap 明暗／Material 主题与桌面、窄屏回归通过。 |
| VIN 不保存、无法继续 | 已保留 409 初败：[uhttpd](https://github.com/openwrt/uhttpd/blob/master/ubus.c#L590-L611)附加 `ubus_rpc_session`，[rpcd exec](https://github.com/openwrt/rpcd/blob/master/plugin.c)转发后被严格业务参数校验拒绝。仅 root-only 本地 ctl 入口移除此元数据；远程管理仍拒绝它与未知业务字段。真实配置落盘、重载与身份复用通过。 |
| 其他页面是否受同因影响 | 影响 LuCI 的 9 个 Tesla 写方法：configure、connect、disconnect、pair、cancel、climate_pair、climate_query、climate_command、ultra_control（均有 `tesla_` 前缀）；status 不走该校验。其他 ctl、网络、更新与 shell 管理入口未发现相同冲突，另以真实 Unix socket 覆盖 6 个管理读取。 |
| Tesla 设置内配对管理 | CarPlay“设置 → Tesla 车辆”（`/car#settings/tesla`）与画面、麦克风共用侧栏；车辆管理、配对、空调控制为同一面板内的独立视图，LuCI 入口也指向这里。沿用独立管理令牌，面板内解锁。VIN 保存、启用、配对、确认、连接完整贯通；取消／失败／超时保留身份，可重试；离页停止轮询并取消待确认操作。启用方式见 [安装说明](INSTALL-OPENWRT.md#tesla-管理入口)。 |
| 彻底删除网页版本拦截 | 旧 WSS 对 BUILD_ID／PROTOCOL 的精确相等要求会直接阻断接入，与访问码无关，不能仅凭提示断定是缓存。按要求完全删除版本字段发送、比较、阻断状态与提示、刷新分支及旧测试语义；全局搜索无对应执行路径。构建号只作诊断展示；Origin、Cookie、管理令牌、消息格式、首消息期限与单 viewer 规则保持。 |
| 直接清理页面后访问 | 连接页及“设置 → 关于”提供“清理页面缓存并重新打开”，也可直接访问 `/car/reset`。返回 303、no-store 和 [Clear-Site-Data 的 cache 类型](https://w3c.github.io/webappsec-clear-site-data/)，跳到唯一同源页面。真实 Chrome 136 验证同一资源 URL 从缓存旧值变为重新请求，身份与本地偏好保留；不修改服务端配对、密钥或访问选择。 |
| 离线提示与恢复 | 真正断网的容器中 `navigator.onLine=false`，本地可信 HTTPS 仍可达；旧代码却据此不发起 WSS。移除该硬拦截，健康通道不因 offline 提示断开；另修复旧心跳清理逻辑导致无 pong 时不重连的问题。黑洞初败保留，修复后 5 秒无有效 pong 启动恰好四次有界重试，显式恢复完成新握手。 |

浏览器证据与边界：

- [Tesla 官方说明](https://www.tesla.com/support/infotainment)确认早期 Model S／X 信息娱乐升级会改变浏览器能力；[Synacktiv Model 3 研究第 6 页](https://www.synacktiv.com/sites/default/files/2023-11/tesla_codeblue.pdf)记录 Intel Atom／AMD 硬件；[Model Y 固件 2020.48.26 的亲测](https://forum.kirupa.com/t/tesla-web-browser-details-benchmarks/645505)记录 Chromium 79。这些资料不能形成所有车型／固件的浏览器版本矩阵，当前车辆具体 MCU、固件和实际 UA 未现场核实。
- 本次严格模拟用户给定目标：实际运行 [官方 Chrome for Testing 136.0.7103.92](https://googlechromelabs.github.io/chrome-for-testing/136.0.7103.92.json) Linux x86_64，以 Browser.getVersion 验证引擎；设置所给 UA、CSS 1254×784、DPR 1.53，实测 DPR 为 float32 值 1.5299999713897705。1920×1200 物理尺寸与 CSS×DPR 有取整差异，不当作实车测量。
- 贯通环境为非 root、无外网、保持 Chrome 沙箱的临时 Docker；测试 CA 仅在容器信任库，不忽略证书错误。真实 Rust HTTPS/WSS、API、配置文件、配对状态机、90 秒期限及 GATT 请求／清理；D-Bus 总线、车辆确认和空调回复为合成夹具。原生 VideoDecoder 解码合成 H.264 并检查实际 canvas 像素，未替换解码器；不代表 Tesla GPU 或完整车辆密码协议验收。

| 最终检查 | 结果 |
| --- | --- |
| 前端／版本标识 | 锁定 TypeScript 5.9.3／Vite 8.3.3 构建通过；诊断 BUILD_ID 两端一致，包版本为 0.0.2-r7 Pre-release，网页接入不按版本标识拦截。 |
| 本机 Rust／页面 | Rust 450 通过、2 ignored（外网与需显式运行的隔离浏览器用例）；Chrome 157 回归覆盖原生 Tesla 设置、管理、缓存、手机／车机配对、访问／心跳、投屏／触控／连续控制及 LuCI。 |
| Chrome 136 真实后端贯通 | CarPlay 原生 Tesla 设置内完整通过，108.46 秒：VIN 落盘重载、取消／失败／实际 90 秒超时／重试成功、Driver 管理、HTTP 缓存清理和原生 H.264 绘制。GATT 启动／清理均 12 次、最终活动 0、授权请求 5 次；缓存探针服务请求从 1 次变 2 次。 |
| Linux 全量／Chrome 136 页面 | 修复阶段 Rust workspace 447 项及 CLI 集成 4 项，共 451 项通过；默认 2 ignored，隔离浏览器用例另已显式通过。实际 Chrome 136 回归覆盖原生 Tesla 设置与同组关键页面；LuCI 主题同步改为有界检查当前控件／样式，保留原断言，解决等待旧动画 Promise 的挂起。 |
| AArch64／APK | 固定 arm64 Alpine 工具链构建通过：静态 AArch64 ELF、双 APK、私有 BlueZ／D-Bus 运行时闭包校验；一次性 OpenWrt 根内 core 升级／卸载／重装、LuCI 安装脚本及合成状态保留通过，无服务启动。 |
| 包生命周期／元数据 | 临时根生命周期、版本元数据、文档链接、规则软链接与差异检查通过。诊断并发初败通过进程隔离处理，原严格断言保持。 |
| r7 远程更新来源 | 预发布渠道声明 `0.0.1-r0`、`0.0.2-r0` 至 `r6`；逐版本持久状态读取实现一致，r7 不变更状态格式。旧 `0.0.2-r0` 更新器尚无开发渠道选择，不能据此承诺其能直接发现 Pre-release；其余来源在开发渠道更新。 |

原生设置边界回归覆盖错误令牌、401／屏幕身份变化重新锁定、离页停止轮询、保留温度输入，以及路由切换后迟到解锁、旧状态回滚外层连接标记和旧确认框抢在下一帧发请求；三处竞态均先复现失败，再验证修复，后两处断言分别为状态不回滚与零写请求。

初败已保留：VIN 元数据 409、旧版本闸门、离线硬拦截、心跳黑洞，以及测试证书 DN／DPR float32／缓存夹具模块路径、LuCI 主题无界等待问题；修正测试前提后才计通过，未放宽断言或延长期限。显式贯通入口（仅隔离 Linux 容器，需 Node、NSS certutil 和准确 Chrome 136）：`COWPLAY_ISOLATED_BROWSER=1 CHROME_BIN=/path/to/chrome cargo test --locked browser_uses_real_tesla_api_persistence_and_pairing_lifecycle -- --ignored --nocapture`。

仍需现场验收：实际安装包与运行进程、车辆 HTTPS 信任、Tesla 车内 BLE 确认与真实读数、iPhone 蓝牙／iAP2／Wi-Fi／CarPlay、车机实际绘制和声音路由。本轮未操作实车或 F50；模拟通过不等同真车验收。
