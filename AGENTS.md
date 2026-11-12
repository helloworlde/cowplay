# AGENTS.md — CowPlay 协作规则

本文件是仓库 AI 指引的唯一来源；`CLAUDE.md` 必须保持相对软链接 `AGENTS.md`。只编辑本文件。平台规则和当前用户明确要求优先；历史授权、装机状态及测试结果不代表当前授权或事实。

## 项目入口

CowPlay 是 Rust 无线 CarPlay 接收端，主要目标为 MU300 OpenWrt / 中兴 F50；Linux 通过显式系统 BlueZ 后端运行。网页为 TypeScript/Vite，不开发 Android 应用。核心能力与验收边界见 [README](README.md)。

| 位置 | 用途 |
| --- | --- |
| `docs/ARCHITECTURE.md` | 模块与平台边界 |
| `docs/RELEASES.md`、`docs/INSTALL-OPENWRT.md`、`docs/LINUX.md` | 构建、安装、签名、更新与恢复 |
| `src/service.rs`、`src/platform/` | 生命周期与 F50／Linux 平台 |
| `src/bluetooth*`、`src/phone_flow.rs`、`src/rfcomm.rs` | 手机配对、认证与会话交接 |
| `crates/core/`、`crates/server/` | 协议、认证、媒体、车辆模型和共享 Feed |
| `src/admin/`、`src/tesla_ble/`、`web/src/`、`luci-app-cowplay/` | 管理、车辆传输与页面 |
| `scripts/`、`packaging/`、`tests/` | 构建、安装与回归 |
| `NOTICE.md`、`packaging/RUNTIME-NOTICE.md`、`research/` | 许可、归属和供应链来源 |

## 工作方式

- 用中文，先报告结论、未满足项和必要证据，区分事实、推断与未知。“分析／评估”默认只读；修改授权不包含提交、合并、推送、发布或设备操作。
- 编辑前检查分支、Git 状态、工作树和进行中任务。并行编辑使用独立工作树，不覆盖、回退、stash、提交或清理他人内容。
- 最小修改，复用现有实现。未经授权不加功能、配置、依赖、无关重构或假设性防御。删除前确认用途、引用和可恢复性；独有用户资料或无法恢复的数据逐次确认。
- 排查先保留初败、核对版本和原因，再修复。复用必要回归，不放宽断言、延长超时或反复重跑掩盖失败。复杂或高风险改动安排独立评审。
- 交付可审查差异、验证环境与限制，区分实现、本地验证和现场验收。公开图片只使用明确标注、经审查的演示数据，不含现场地址、真实数据、日志或私人截图；本地交接和审查过程不进入公开文档。
- 保留用户指定的 `hellowoodes@gmail.com` 作者邮箱，不擅自改写历史身份。开发版本标为 Pre-release，正式发布须单独确认；更新器的正式／开发渠道不能混用。

## 设备与私有状态

- 安装／卸载、启停服务、配对窗口和手机连接／切换／断开均需当前适用授权，并核对设备占用。无明确授权不刷机、不改启动项、自启、热点、蜂窝、路由、温控、看门狗或清配对。
- 保留 `/etc/cowplay/state` 中的用户配置、身份、配对和证书。版本变更先核对状态格式；数据删除与降级需要独立授权。
- F50 使用厂商 5.4 内核，操作前核实；禁止安装官方 6.12 内核模块。私有 BlueZ/D-Bus 位于 `/usr/lib/cowplay/bluez`，不替换系统服务、驱动或固件。F50 USB 无主机能力。
- 配件身份仅使用合法持有资料。私钥、配对、Wi-Fi 密码、令牌、VIN 和 DDNS 凭据留在私有目录，不进入源码、包、日志、截图或对外诊断；测试用合成数据。保持 HTTPS、Origin、访问及管理认证，不绕过证书错误。

## 实现与验证

- 保留 Rust workspace、TypeScript/Vite 与锁文件。新增／替换依赖先说明必要性、许可和影响并取得授权，不随意升级或重建锁文件。
- 页面共同行为复用 `web/src/shared/`；修改 `web/src/` 后构建生成 `web/dist/`，不手改产物。前后端 `BUILD_ID`、`PROTOCOL` 保持一致；版本核对 `Cargo.toml`、`packaging/VERSION` 与包脚本。
- 配对、连接与重试保持有界、可取消、可清理；失败不清配对。会话切换验证旧会话释放、迟到响应隔离、停止再连和失败恢复。
- 分别报告网页、蓝牙/iAP2、Wi-Fi、CarPlay 会话、收到媒体与浏览器实际绘制；进程存活、HTTP 成功或手机出声不能代替功能验收。

从仓库根目录按影响执行，依赖缺失才运行 `npm ci --prefix web`：

| 影响 | 验证 |
| --- | --- |
| Rust／协议／服务 | `cargo test --workspace --locked`；需回环 TCP/UDP，沙箱限制如实报告 |
| 前端与嵌入资源 | `npm run build --prefix web`，随后构建／测试受影响 Rust |
| 配对／切换 | 先构建前端，再 `node scripts/test-pairing-web.mjs` |
| 投屏／触控／恢复 | 先构建前端，再 `node scripts/test-viewer-web.mjs` |
| 包生命周期 | `python3 scripts/test-package-lifecycle.py` |
| 发布附件／签名 | `python3 scripts/test-release-artifacts.py`；需 APK v3 与 OpenSSL |
| 文档／规则 | 核对链接、命令和差异；`readlink CLAUDE.md` 为 `AGENTS.md`，`cmp AGENTS.md CLAUDE.md` 成功 |

浏览器回归需本机 Chrome；LuCI 回归需 `LUCI_RUNTIME_DIR`。AArch64/APK 按现有容器工具链构建并验证静态 ELF 与私有运行时闭包。`test-lifecycle.sh`、`test-device-service.sh`、`test-installed-package.sh` 会操作服务或包，先读脚本并核对授权；APK 安装测试只在一次性隔离根运行。合成测试、短时启停均不能代替真实 iPhone、双手机切换、热点中断、长期运行、音频或车机验收。
