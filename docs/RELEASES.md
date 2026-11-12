# 构建、签名与发布

首次安装按 [LuCI 安装步骤](INSTALL-OPENWRT.md) 执行。F50 自行访问公开的 GitHub Release，不保存 GitHub 私人令牌。车机和 Tesla 管理入口也列在该文档。

## 版本与检查更新

`packaging/VERSION` 是双 APK 与 Linux 归档的版本源，与 Cargo 版本保持一致；修订号为 0 时标签省略 `-r0`，非零修订使用 `vX.Y.Z-rN`。前后端 BUILD_ID 与 PROTOCOL 必须一致。

在 LuCI“服务 → CowPlay → 系统 → GitHub 安装与升级”选择“正式版”或“开发版（Pre-release）”。正式渠道只接受正式版，开发渠道只接受预发布；指定标签也必须属于所选渠道。切换渠道后重新检查，确认安装时显示候选渠道。检查不会安装，安装需确认。

正式渠道查询 GitHub 最新正式版；开发渠道从最近 100 个发布中选取最高规范版本，响应上限 8 MiB，不回退到验签失败版本之前的旧包。任务保存渠道，安装前再次核对该标签的渠道、签名、架构和状态兼容性。后台任务独立于投屏服务，浏览器关闭、服务停止或 rpcd 重载后可恢复读取状态。切换渠道不授权降级。

首次引导验证脚本哈希、公钥指纹、签名清单和辅助程序后建立信任。不同的既有来源或公钥不会被静默替换。后续更新仅从预置仓库下载，使用系统 CA 和 GitHub 域名限制；不忽略 TLS 错误。

安装先验证签名、大小、SHA-256、包名、双包版本、架构、OpenWrt 版本、空间和依赖，再停止原服务，在设备本地备份私有状态、UCI 与 APK world，执行双包安装并恢复原运行／自启选择。失败保留任务与备份，不自动回滚状态或盲目降级。备份不上传 GitHub。

本版不自动启用 `cowplay-restore` 服务，也不承诺固件升级后自动重装。固件升级、刷机和内核变更需独立验证；F50 厂商 5.4 内核不能安装官方 6.12 内核模块。

## Actions 与发布渠道

1. main、PR 或手动触发会在 x86_64、ARM64 原生 runner 构建静态程序、网页和安装材料，运行 Rust、浏览器与安装相关检查。
2. ARM64 构建 OpenWrt 双包并进行隔离安装与生命周期验证。
3. 汇总实际产物，用一次性测试密钥验证完整发布清单。测试私钥和测试签名附件不上传。
4. 与包版本匹配、位于 main 历史的标签才进入 Release 任务。`packaging/release-policy.json` 的 `channel` 必须明确为 `dev` 或 `stable`。当前为 `dev`，发布成 Pre-release 且不标记 Latest；正式版本须在验收和维护者确认后显式选择 `stable`。缺失或未知渠道使构建失败。发布任务验证生产公钥与仓库公钥完全一致，签名双 APK、发布清单和 SHA256SUMS。
5. 正文、首次安装命令与 GitHub 预发布标记使用同一渠道策略，不自动汇入提交、PR 或现场记录。本地完成签名与哈希验证，草稿上传后下载全部附件，以 `diff --recursive` 与本地已验证目录逐字节比较，同时拒绝缺失、多余或变更的文件；通过后才发布，并显式设置渠道与 Latest。已发布附件不可覆盖；修复使用新版本号。现有版本分类纠正使用原位元数据修改，保留签名附件。

生产密钥配置为 GitHub Secret `COWPLAY_RELEASE_SIGNING_KEY`，公开 PEM 的 SHA-256 配置为 Variable `COWPLAY_RELEASE_PUBLIC_KEY_SHA256`。`release` Environment 的审批规则由仓库管理员按需设置；工作流引用 Environment 不等于已设审批规则。签名私钥只临时写入 runner 私有目录，退出删除，签名容器禁网络。设备只接收公钥。

工作流使用 [GitHub 官方 Action 并固定完整提交 SHA](https://docs.github.com/en/actions/reference/security/secure-use#using-third-party-actions)，原生 `docker://` Action 运行固定摘要的 Rust 镜像，官方 Action 管理 Node、Cargo 依赖缓存与产物传递。组合命令集中在 Makefile：`check-release`、`ci-web`、`ci-browser`、`ci-build`、`release-test`、`release-sign` 和 `release-publish`，本地容器入口也复用原生打包目标。发布使用 runner 自带的官方 `gh`，签名与兼容清单复用 APK v3／OpenSSL。增加外部发布 Action 会使其代码及依赖获得发布任务权限，固定版本仍需审查源码；当前未引入此类依赖。镜像摘要不固定 runner、npm 审计数据或 Alpine 在线安装的软件包，不能据此宣称完全可复现。

附件固定为 13 个安装与验证文件：

| 内容 | 数量 | 必要性 |
| --- | --- | --- |
| OpenWrt 核心与 LuCI APK | 2 | F50 安装与管理 |
| `packages.adb` | 1 | 签名 APK 索引，通过 GitHub Release 按包名安装 |
| Linux x86_64、ARM64 归档 | 2 | 对应平台安装，含许可证与归属 |
| `release-key.pem` | 1 | 验证发布签名，使用前独立核对指纹 |
| `cowplay-release.json`、`cowplay-release.sig` | 2 | OpenWrt 更新的签名兼容清单 |
| `SHA256SUMS`、`SHA256SUMS.sig` | 2 | 所有附件的完整性与真实性 |
| Linux、OpenWrt 下载脚本 | 2 | 经过校验的安装入口 |
| `cowplay-updater-aarch64` | 1 | 首次 OpenWrt 安装的验签与事务辅助程序 |

LuCI 复制命令放在 Release 正文，不再另发文本附件。发布正文使用 `--notes` 输出到附件目录之外，工作流仅上传附件目录。回归验证精确文件集合、完整校验覆盖和测试私钥不进入输出。GitHub 自动提供的源码归档不属于自定义附件。

附件没有设备身份、私钥、配对、配置备份、现场日志或截图。公开源码仅包含运行、构建、回归、使用与必要供应链材料；README 图片不得包含私人资料。开发现场记录和本地交接过程不进入公开仓库。保留维护者指定作者邮箱，其他身份与私有内容分别审查；删除当前文件不会清除 Git 历史。

当前验证状态以 [Actions](https://github.com/helloworlde/cowplay/actions) 为准。隔离测试不代表真实 iPhone、车辆、音频、热点、长期运行或固件升级验收。原生 CarPlay 图标慢／缺失尚缺现场资源记录，不能把网页图标缓存修复视为该问题已解决。
