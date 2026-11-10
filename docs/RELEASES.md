# GitHub Release 安装、更新与系统升级恢复

发布来源为 `helloworlde/cowplay`。核心、LuCI 和 Linux 包共用 `packaging/VERSION`；设备端支持公开的稳定 Release，不保存 GitHub 访问令牌。

## 应用更新

LuCI「系统 → GitHub 安装与升级」可检查最新稳定版或指定 `vX.Y.Z-rN` 标签。任务由 `cowplay update` CLI 和独立后台进程执行，不依赖正在运行的 CarPlay 服务；服务停止、rpcd 重载和浏览器刷新后仍可读取持久记录。只接受稳定发布，拒绝自动降级。已安装核心/LuCI 任一版本高于目标、未列入 `upgrade_from`、架构或固件版本不匹配时均拒绝。

信任配置由设备管理员预置，不允许网页提交任意下载 URL 或换公钥：

- `/etc/cowplay/releases.json`：内容为 `{"repository":"helloworlde/cowplay"}`；可从包内 `releases.example.json` 复制后核对。
- `/etc/cowplay/release-key.pem`：通过可信独立渠道核对指纹的发布公钥。
- 两个文件须为 root 所有且不可由组/其他用户写入。`/etc/cowplay/updates`、恢复缓存和私有状态目录均应为 `0700`，私有文件为 `0600`。

下载只使用通过系统 CA 验证的 GitHub HTTPS，禁用环境代理，不忽略证书错误；重定向限制到 GitHub 与其 Release 资产域名。API、清单、签名、单包均有体积/时间上限。失败显示阶段和原因，不把地址、身份或令牌上传给 GitHub。

安装顺序：

1. 获取 GitHub 发布信息与 `cowplay-release.json`、`cowplay-release.sig`。
2. 使用预置公钥验证清单的 OpenSSL SHA-256 签名，核对仓库、标签、版本、系统、架构及来源版本兼容声明。
3. 明确确认安装后，下载同版本核心与 LuCI，核对签名清单中的 SHA-256/大小，再用原生 APK v3 验证 APK 签名并读取内嵌包名、版本、架构和安装体积。
4. 预检空间、离线模拟依赖解析；下载后及安装前重新核对已安装版本。与端口接管任务互斥，LuCI 写操作在更新状态未知或活动期间停用。
5. 如原服务运行，识别旧 PID、停止并等待旧进程及 HCI 释放。停服失败或超时不安装。
6. 停服后把现有 `/etc/cowplay/state`、`/etc/config/cowplay`、`/etc/apk/world` 存入设备本地 `updates/<id>/backup.tgz`。不上传备份；外置路径引用不在此快照内，需管理员另行保管。
7. 将系统恢复缓存标记为 blocked，执行严格验签、禁网络的双包安装；不使用 `--allow-untrusted`，不安装固件、内核模块或替换系统 BlueZ。
8. 检查两包版本、自启偏好，恢复原运行状态并检查管理接口。首次核心安装只开启下次启动，不立即 start。保存新版本系统恢复缓存后清除 blocked，保留 journal 和备份并清理临时下载文件。

检查结果 15 分钟内可用于安装，之后需重查。核心与 LuCI 已为所选版本时显示 current；可准备固件恢复缓存，不伪装执行重装。全新设备可直接导入发布公钥，再用原生 `apk add` 安装同版本双包，无需预先放置 CowPlay 二进制。

### 首次安装：直接从 GitHub 下载公钥和 APK

首次安装由设备直接从 `helloworlde/cowplay` 的同一个 GitHub Release 标签下载 `release-key.pem`、核心 APK 和 LuCI APK，再导入公钥并执行 `apk add`，不要求先用电脑下载或上传文件。Release 工作流已经包含公钥和双包附件。仓库当前私有且尚无 Release，下面流程在仓库公开并发布正式签名 Release 后可用；本地未签名候选包不能用于该流程。

需要支持 APK v3 的匹配 OpenWrt，以及可用的官方系统包源。公钥文件本身也从 GitHub 下载；其预期 SHA-256 指纹须先通过可信渠道核对，不能仅凭同次下载的一份校验值建立信任。指纹可随管理员确认的安装说明提供，不需要手工传输公钥文件。设备只接收公钥，发布私钥留在签名端。

以下示例在设备上以 root 执行，版本固定为同一标签，避免下载期间 latest 改变导致两个包版本混用。命令由管理员在匹配的已授权设备上执行。

```sh
(
    set -eu
    version=0.3.14-r17
    expected_key_sha256='填写已核对的64位发布公钥SHA256指纹'
    case "$expected_key_sha256" in
        ''|*[!0-9a-f]*) echo '请先填写已核对的小写十六进制公钥指纹' >&2; exit 1 ;;
    esac
    [ "${#expected_key_sha256}" -eq 64 ] || exit 1

    # 从设备现有的官方签名包源取得下载工具和系统 CA。
    apk add ca-bundle curl
    work=$(mktemp -d /tmp/cowplay-install.XXXXXX)
    trap 'rm -rf "$work"' EXIT HUP INT TERM
    base="https://github.com/helloworlde/cowplay/releases/download/v$version"
    core="cowplay-$version-aarch64_generic.apk"
    luci="luci-app-cowplay-$version-noarch.apk"
    for file in release-key.pem "$core" "$luci"; do
        curl --fail --location --proto '=https' --proto-redir '=https' \
            --connect-timeout 15 --max-time 180 \
            "$base/$file" -o "$work/$file"
    done
    printf '%s  %s\n' "$expected_key_sha256" "$work/release-key.pem" | sha256sum -c -

    mkdir -p /etc/apk/keys
    cp "$work/release-key.pem" /etc/apk/keys/cowplay-release.pem
    chmod 0644 /etc/apk/keys/cowplay-release.pem
    apk verify "$work/$core" "$work/$luci"
    apk add "$work/$core" "$work/$luci"

    # 为安装后的 LuCI 在线更新保存同一公钥和 GitHub 来源。
    mkdir -p /etc/cowplay
    cp "$work/release-key.pem" /etc/cowplay/release-key.pem
    chmod 0644 /etc/cowplay/release-key.pem
    cp /usr/share/cowplay/releases.example.json /etc/cowplay/releases.json
    chmod 0600 /etc/cowplay/releases.json
)
```

`curl` 使用系统 CA 验证 HTTPS，不关闭证书检查。三个文件都从同一 GitHub Release 下载；任何下载或指纹验证失败都会停止。导入公钥时保留原有 OpenWrt 公钥，`apk add` 可继续从官方系统源解析和验证缺少的依赖；依赖已齐全时，最后的安装命令可加 `--no-network`。整个安装过程不使用 `--allow-untrusted`，也不下载并执行远程 shell 安装器。

APK 原生校验签名、包架构和依赖，但不代替发布清单中的固件版本/状态兼容性检查，管理员仍需选择匹配固件的 Release。安装后可重新登录 LuCI。首次核心安装只开启下次开机自启，不立即启动；身份、证书和私有配置仍须按项目安装要求准备。

`/etc/apk/keys/cowplay-release.pem` 供系统 APK 使用；`/etc/cowplay/release-key.pem` 供 CowPlay 验证发布清单与更新包。示例已配置两处公钥和 `helloworlde/cowplay` 来源，后续可在 LuCI 检查更新。首次手工 `apk add` 不会自动准备签名恢复缓存，需检查当前已安装版本后点击「准备系统升级恢复缓存」。

已有经可信渠道核验的静态 CowPlay 二进制时，也可使用下面的 CLI 方式，作为替代入口；不执行未经验证的远程安装脚本。

```sh
# 管理员仅在获授权设备上执行。
# /tmp/cowplay-bootstrap 必须已通过可信独立渠道完成完整性核验。
/tmp/cowplay-bootstrap update check </dev/null
/tmp/cowplay-bootstrap update status </dev/null
printf '%s\n' '{"id":"检查结果中的32位任务编号"}' | /tmp/cowplay-bootstrap update apply
```

任务记录位于 `/etc/cowplay/updates/<id>/job.json`。后台退出或设备重启后，非终态任务在启动宽限期后显示 interrupted。安全终态的残留锁自动清理；停服/安装后的失败保留锁与备份。管理页「检查恢复后解除锁定」仅确认管理员已检查设备，不修复包、不恢复旧状态、不启动服务。需要通过 SSH 查看 APK 状态和系统日志；优先安装已审查的相同或更新版本。旧程序可能不兼容已变化的状态，因此不自动降级或覆盖状态目录。

外部 SSH/APK/系统软件包管理器不受本页面事务锁约束，不应与更新并行使用。更新前后可能出现部分安装，APK 双包操作不等于断电原子事务。下载/验签/依赖失败不操作服务；备份等安装前失败可请求恢复旧服务；一旦开始 APK 写入，失败后保持待人工检查。

## OpenWrt 升级后如何保留

原代码没有系统升级恢复支持。应用包的 `apk add --upgrade` 会保留私有状态，但普通 `sysupgrade` 不承诺保留手工安装的软件包；`-k` 只记录已安装包名，不自动重装。GitHub Release 附件也不是可直接交给官方 Attended Sysupgrade 的包仓库。[OpenWrt 25.12 sysupgrade 源码](https://raw.githubusercontent.com/openwrt/openwrt/openwrt-25.12/package/base-files/files/sbin/sysupgrade) 展示了配置保留与包列表的区别。

本实现增加：

- `/lib/upgrade/keep.d/cowplay`：保留 UCI、私有状态、来源、公钥、签名恢复缓存、恢复 helper、本清单和相关启动链接。不复制旧 `/lib/apk/db`，不把旧主程序直接塞回新系统，不保留整个更新历史或所有备份。
- `/lib/upgrade/cowplay.sh`：生成系统升级备份时，若应用/端口事务未结束，或缓存与升级前安装的两包版本不一致，保存 blocked 标记到备份，阻止启动时恢复过期软件包。包括通过外部 APK 更换版本的场景。`sysupgrade -l/-b` 也会调用该安全检查，发现不安全缓存时会将其锁定；核对应用状态并重新准备当前版本缓存后才能解除。
- `/etc/init.d/cowplay-restore`：核心安装/升级时只启用恢复 helper 的下次启动，不立即执行；位于 CowPlay S95 之前的 S94。
- `/usr/libexec/cowplay-restore`：新系统启动时最多尝试一次离线恢复。先检查 blocked/活动任务、公开来源、清单签名、架构、清单声明的固件版本、双包哈希与 APK 签名，再模拟依赖和安装。已有相同双包时不重复安装；存在不同版本时不覆盖。

每次通过更新器成功安装都会更新 `/etc/cowplay/firmware-restore`。仅保留一个当前完整缓存；指针原子更新，新缓存落盘后才清除 blocked。旧缓存不能在未完成的应用升级后恢复旧程序。已有手工安装的新版，可检查其对应标签，在 current 状态点击「准备系统升级恢复缓存」；此操作不停止服务。CLI 对应 `update prepare`，输入为检查任务 ID。

需要重装时，恢复器按保留的最新 `/etc/rc.d/S95cowplay` 启动链接恢复自启选择，不使用早先准备缓存时的旧快照。已有相同版本双包时不修改服务或自启；若新固件自身预装 CowPlay，还需在该固件构建策略中验证自启默认值与恢复配置的关系。恢复器不直接 start；正常开机流程根据恢复的启动偏好启动服务。私有身份、配对、证书和配置保留。卸载核心会禁用恢复 helper，避免下次开机擅自重装已卸载应用。

支持范围与限制：

- 仅支持**保留设置、清单明确声明兼容、架构匹配且依赖齐全**的固件升级。当前发布策略仅声明 OpenWrt `25.12.6`、`aarch64_generic`；跨版本必须先验证并在签名清单 `openwrt_releases` 中明确列出，不能自动推断 ABI/厂商驱动兼容。
- `sysupgrade -n`、恢复出厂或不保留本清单的厂商刷机工具会清除状态与缓存，不支持自动恢复。
- 恢复缓存只含 CowPlay 双包，不缓存整个固件依赖闭包。新固件缺少 `openssl`、`jsonfilter`、LuCI/核心依赖或 APK v3 时安全失败，应用可能尚未装回，但私有状态与签名缓存仍在；管理员需先安装兼容依赖或选择包含它们的正确固件，再运行 `/usr/libexec/cowplay-restore` 重试。
- 结果写入缓存下 `result` 并通过系统日志与 LuCI 展示。恢复失败不自动清理缓存，不联网获取任意依赖，不尝试刷机。
- 所有恢复数据包含设备私有资料，系统升级备份也必须留在可信私有位置，不可上传 Release、Issues 或 Actions artifacts。
- 不能将 F50 的厂商 5.4 内核/驱动迁移到官方 6.12 固件。本能力不代表已验证任意固件或真实系统升级；真实固件升级需要单独验收。

## GitHub Actions 构建与签名发布

工作流在 main、PR 和手动触发时执行：

1. x86_64、ARM64 原生 runner 分别构建前端和静态 Rust 程序，运行 Rust、前端逻辑、Linux 安装与签名下载测试，生成 Linux tar.gz。
2. ARM64 复用同一已构建静态程序生成 OpenWrt 双 APK，验证包生命周期与恢复机制。对本次真实双包使用临时测试密钥签名、导入公钥，隔离安装并执行 LuCI 安装脚本、卸载和重装检查。
3. 汇集三平台实际产物，以临时测试密钥执行完整发布打包和验签。测试私钥与测试签名附件不上传。
4. 仅当 `vX.Y.Z-rN` 标签与 `packaging/VERSION` 一致且位于 main 历史时，才进入受保护的生产 release job。

正式发布先核对生产公钥指纹，再签名两个 APK、两个 Linux 归档、OpenWrt 清单和 SHA256SUMS。上传十个白名单附件到草稿 Release，重新从 GitHub 下载，检查附件集合、签名和每个文件的哈希，全部正确后才公开。失败草稿可重跑续传；已公开 Release 禁止覆盖。草稿含额外附件时拒绝发布，管理员须先核对并清理这些附件。

首次发布需要仓库管理员完成：

1. 在可信私有环境创建专用 RSA 发布密钥，不复用设备 HTTPS/MFi/Tesla 密钥：

   ```sh
   umask 077
   openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:3072 -out cowplay-release.key
   openssl pkey -in cowplay-release.key -pubout -out release-key.pem
   openssl pkey -in cowplay-release.key -pubout | shasum -a 256
   ```

2. 创建并保护 GitHub `release` Environment，限制发布标签及审批权限，并保护 main 和版本标签。工作流的 environment 声明本身不创建审批规则。
3. Environment Secret `COWPLAY_RELEASE_SIGNING_KEY` 保存专用 PEM 私钥；Environment Variable `COWPLAY_RELEASE_PUBLIC_KEY_SHA256` 保存上述公钥 PEM 字节的 SHA-256 小写十六进制。使用 `GITHUB_TOKEN` 上传，不需要个人访问令牌。
4. 更新 Cargo 版本、`packaging/VERSION` 与前后端 BUILD_ID；在 `packaging/release-policy.json` 只列出已经验证的来源版本和固件。提交并等待 main CI 成功后，再推送对应版本标签。
5. 通过独立可信渠道提供公钥指纹。设备只接收公钥，密钥轮换需重新建立信任。私有仓库的匿名设备下载不可用，公开下载须先公开仓库并发布正式 Release。

生产私钥只临时写入 runner 私有目录，步骤退出删除；APK 签名容器禁网络。Release 与 Actions artifacts 不包含设备状态、备份、配对、证书、日志或整个工作树。Linux 安装说明见 [Linux](LINUX.md)。

## 验证范围

CI 检查版本一致性、前端编译与逻辑、Rust workspace、包布局和权限、真实 APK 签名与隔离安装、Linux 安装与恢复门禁、下载验签与归档拒绝、系统升级缓存和完整发布资产签名。具体结果以 [Actions](https://github.com/helloworlde/cowplay/actions) 的提交记录为准。

CI 不操作真实 F50 或 Linux 服务，不代表已通过真实 rpcd 重载、断电、固件升级、iPhone、热点、音频或媒体验收。这些现场检查需要管理员安排，不以进程存活或隔离测试代替。
