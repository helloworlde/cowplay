# 构建、签名与发布

首次覆盖安装按 [LuCI 安装步骤](INSTALL-OPENWRT.md) 执行。F50 自行访问公开的 GitHub Release，不保存 GitHub 私人令牌。车机和 Tesla 管理入口也列在该文档。

## 版本与检查更新

`packaging/VERSION` 是双 APK 与 Linux 归档的版本源。首发：Cargo `0.0.1`、APK `0.0.1-r0`、Git 标签 `v0.0.1`；修订号为 0 时标签省略 `-r0`，非零修订使用 `vX.Y.Z-rN`。前后端 BUILD_ID 与 PROTOCOL 必须一致。

LuCI“服务 → CowPlay → 系统 → GitHub 安装与升级”查询最新稳定版本或指定标签。检查不会安装，安装需确认。后台任务独立于投屏服务，浏览器关闭、服务停止或 rpcd 重载后可重新读取持久状态。

首次引导验证脚本哈希、公钥指纹、签名清单和辅助程序后建立信任。不同的既有来源或公钥不会被静默替换。后续更新仅从预置仓库下载，使用系统 CA 和 GitHub 域名限制；不忽略 TLS 错误。

安装先验证签名、大小、SHA-256、包名、双包版本、架构、OpenWrt 版本、空间和依赖，再停止原服务，在设备本地备份私有状态、UCI 与 APK world，执行双包安装并恢复原运行／自启选择。失败保留任务与备份，不自动回滚状态或盲目降级。备份不上传 GitHub。首发已验证从旧开发版覆盖程序并保留状态；不进行数据格式迁移。

本版不自动启用 `cowplay-restore` 服务，也不承诺固件升级后自动重装。固件升级、刷机和内核变更需独立验证；F50 厂商 5.4 内核不能安装官方 6.12 内核模块。

## Actions 与正式发布

1. main、PR 或手动触发会在 x86_64、ARM64 原生 runner 构建静态程序、网页和安装材料，运行 Rust、浏览器与安装相关检查。
2. ARM64 构建 OpenWrt 双包并进行隔离安装与生命周期验证。
3. 汇总实际产物，用一次性测试密钥验证完整发布清单。测试私钥和测试签名附件不上传。
4. 与包版本匹配、位于 main 历史的标签才进入正式 Release 任务。发布任务验证生产公钥与仓库公钥完全一致，签名双 APK、发布清单和 SHA256SUMS。
5. 上传草稿后从 GitHub 重新下载全部附件，比较文件集合、签名和每个哈希，全部通过后发布。已发布版本不可覆盖；修复使用新版本号。

生产密钥配置为 GitHub Secret `COWPLAY_RELEASE_SIGNING_KEY`，公开 PEM 的 SHA-256 配置为 Variable `COWPLAY_RELEASE_PUBLIC_KEY_SHA256`。`release` Environment 的审批规则由仓库管理员按需设置；工作流引用 Environment 不等于已设审批规则。签名私钥只临时写入 runner 私有目录，退出删除，签名容器禁网络。设备只接收公钥。

附件包含双 APK、两种 Linux 归档、公钥、OpenWrt 清单及签名、SHA256SUMS 及签名、Linux 安装脚本、OpenWrt 引导脚本、同构建的 ARM64 辅助程序、LuCI 安装命令。没有设备身份、私钥、配对、配置备份、现场日志或截图。公开源码使用已审查的发布历史，开发现场记录不进入公开仓库。

当前验证状态以 [Actions](https://github.com/helloworlde/cowplay/actions) 为准。隔离测试不代表真实 iPhone、车辆、音频、热点、长期运行或固件升级验收。原生 CarPlay 图标慢／缺失尚缺现场资源记录，不能把网页图标缓存修复视为该问题已解决。
