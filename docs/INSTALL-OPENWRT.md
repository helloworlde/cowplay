# F50 首次覆盖安装与在线更新

首个正式 Release 是 `v0.0.1`，两个 APK 的版本都是 `0.0.1-r0`。安装覆盖 CowPlay 程序与 LuCI 页面，保留配置、配对、身份、证书和原运行／自启选择。安装过程中投屏和声音会中断。无需连接开发电脑，也无需 SSH；F50 自行从 GitHub 下载。

## 首次安装：只有 LuCI 软件包页时

1. 等仓库公开，并确保 F50 可访问 GitHub。打开 LuCI“系统 → 软件包”，更新列表，从设备已有官方源安装 `luci-app-commands` 和 `curl`。不要安装内核模块或更换软件源。
2. 刷新 LuCI，打开“系统 → 自定义命令 → 配置”（英文为 Custom Commands → Configure）。添加一个命令，描述填写“安装 CowPlay”。**不要勾选“公开访问”或“自定义参数”。** 此操作继续使用已有 LuCI 登录和 HTTPS。
3. 从 [v0.0.1 Release](https://github.com/helloworlde/cowplay/releases/tag/v0.0.1) 打开 `install-openwrt-command.txt`，把整行复制到“命令”，保存并应用。该命令包含固定版本、脚本 SHA-256 和发布公钥指纹。
4. 在“自定义命令 → 概览”点击“安装 CowPlay”。页面显示 Started 表示后台任务已启动，**不代表安装完成**。不要重复点击，也不要断电。
5. 可添加第二个命令 `cat /tmp/cowplay-install.log`，名称为“安装进度”，同样保持非公开。运行它查看结果。出现 `CowPlay is ready` 后刷新 LuCI，即可使用新页面。

官方 LuCI 的软件包 URL 输入框不能可靠处理 APK 下载，直接上传陌生公钥签名的包也会被拒绝。因此首次使用上述官方自定义命令入口；不需要关闭 APK 签名、HTTPS 或登录校验。[LuCI 官方说明](https://github.com/openwrt/luci/issues/8410)、[官方自定义命令页面](https://github.com/openwrt/luci/blob/openwrt-25.12/applications/luci-app-commands/htdocs/luci-static/resources/view/commands.js)。

可信发布公钥 SHA-256：

```text
5f926239adc2029a33ed58c6e7e2107dcc424fb7e8133cdbdcb777e5f1a8b40f
```

脚本先验证公钥、签名清单和辅助程序，再下载并验证双 APK；校验和依赖检查通过后才停止 CowPlay。首次引导不替换不同的既有发布来源或公钥。管理员执行固定安装命令即授权该次安装；后续网页更新仍需确认。

## 后续检查与更新

LuCI“服务 → CowPlay → 系统 → GitHub 安装与升级”：

1. 留空“版本标签”，点击“检查 GitHub 版本”，由 F50 查询最新稳定 Release。
2. 有新版本时显示版本与架构；点击“安装所选版本”，确认后执行。
3. 完成后刷新页面。检查本身不会安装；取消确认不会发出安装请求。

下载或校验失败不会停止服务。安装失败会保留设备内备份和任务状态；先核对日志、软件包与服务，不盲目降级或恢复旧状态。首次安装失败且新 LuCI 页面尚未安装时，仍可从“自定义命令”查看安装进度。仓库为私有时匿名检查会失败，不要在设备中保存 GitHub 私人令牌来绕过。

## Tesla 管理入口

- 车机页面：“设置 → Tesla 车辆”。
- LuCI：“服务 → CowPlay”页内导航的“Tesla 车辆”。
- 直接路径：`/cgi-bin/luci/admin/services/cowplay/tesla`，沿用 LuCI 的地址和端口。
- Linux 独立主机：`/manage#tesla-management`，需要管理令牌。

只读监控配对、控制授权和连接状态分开显示；原生 CarPlay UI 可离线浏览，实际控制仍需连接和授权。

## 设备访问码

首次打开全新 CowPlay 时，在车机或手机页面输入两次相同的六位数字，自行设置这台接收端的访问码，无需先登录 LuCI。访问码保存在接收端，重启和覆盖安装不会更换；只有受保护的管理操作才能修改或重置。

每个新浏览器都必须输入正确访问码，错误会拒绝访问。验证通过后分别记住各浏览器；清除浏览器数据或撤销授权后需重新验证。二维码只打开页面，不再跳过验证。

登录 LuCI → 服务 → CowPlay → 设备与访问，可显示、修改或重置访问码。修改/重置后旧码不能再授权新浏览器，已有浏览器和 iPhone 配对保留。Linux 的独立管理页提供相同修改/重置能力，首次使用不依赖该管理页。

已有设备保留原访问码和浏览器授权；不会因为升级、设备列表为空或读取失败重新开放首次设置。旧开发版若仍使用默认码，管理员可修改。此码与蓝牙配对码、Tesla 车辆钥匙授权独立。
