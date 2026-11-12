# Linux x86_64／ARM64 发布与安装

支持普通 Linux 两种架构的构建、签名、发布和安装升级。OpenWrt 使用原签名 APK 通道；Linux 通用包不装入 OpenWrt，不携带 F50 的私有 BlueZ 运行时，也不改变系统蓝牙、热点、驱动或固件。

## 发布附件

| 附件 | 用途 |
| --- | --- |
| `cowplay-<版本>-linux-x86_64.tar.gz` | Intel／AMD 64 位 Linux 静态程序与安装材料 |
| `cowplay-<版本>-linux-arm64.tar.gz` | AArch64 Linux／64 位树莓派系统 |
| `install-linux-from-github.sh` | 验证后使用的 Linux 下载与安装入口 |
| `SHA256SUMS`、`SHA256SUMS.sig` | 覆盖双 Linux 包、下载器、公钥及 OpenWrt 附件的签名 SHA-256 清单 |
| `release-key.pem` | 发布公钥；需要事先核对预期指纹 |

每个 tar.gz 只含固定白名单：静态 `cowplay`（已嵌入前端）、`install.sh`、`cowplay.service`、Linux 配置示例、`VERSION`、`ARCH`、`UPGRADE_FROM`、安装说明和许可证／版权说明。归档为固定顺序、固定时间戳的 USTAR/gzip，不含符号链接、硬链接、设备节点、真实配置、身份、证书、配对或 Git 数据。

构建使用对应架构的原生 Alpine/musl 工具链。打包前运行 CowPlay 自身的静态 ELF 检查：64 位小端、CPU 架构正确、无动态加载器及共享库依赖。原 OpenWrt `verify-image` 默认仍只接受 AArch64；新参数可显式检查 `arm64` 或 `x86_64`。

运行无需 Rust、Node、Python 或编译器。主机仍需现有 BlueZ、D-Bus、系统 CA、合适的 BR/EDR 蓝牙适配器、已经配置的 Wi-Fi 热点，以及用户合法持有的身份与 HTTPS 资料。构建成功和安装器测试不能替代真实手机／媒体验收。不支持 32 位 ARM。

## 从 GitHub 安装

先在主机官方包源安装 `curl`、OpenSSL、系统 CA 及安装所需标准工具。安装器使用 POSIX sh、coreutils（含 `timeout`、`mv -T`）、util-linux `flock`、tar；只有 systemd 模式使用 `systemctl`。不自动运行 apt/dnf 或修改系统蓝牙服务。

以下以 r17 为例；仓库公开并发布正式签名 Release 后才有可下载附件。指纹由管理员从可信渠道核对，不能从同一未验证下载中自动接受新公钥。以下命令由管理员在目标主机上执行。

```sh
(
    set -eu
    umask 077
    version=0.3.14-r17
    expected_key_sha256='填写已核对的64位发布公钥SHA256指纹'
    printf '%s\n' "$expected_key_sha256" | grep -Eq '^[0-9a-f]{64}$'
    work=$(mktemp -d /tmp/cowplay-bootstrap.XXXXXX)
    trap 'rm -rf "$work"' EXIT HUP INT TERM
    base="https://github.com/helloworlde/cowplay/releases/download/v$version"
    for file in release-key.pem SHA256SUMS SHA256SUMS.sig install-linux-from-github.sh; do
        curl --fail --location --proto '=https' --proto-redir '=https' \
            --connect-timeout 15 --max-time 180 --max-filesize 65536 \
            "$base/$file" -o "$work/$file"
    done
    printf '%s  %s\n' "$expected_key_sha256" "$work/release-key.pem" | sha256sum -c -
    openssl dgst -sha256 -verify "$work/release-key.pem" \
        -signature "$work/SHA256SUMS.sig" "$work/SHA256SUMS"
    hash=$(awk '$2 == "install-linux-from-github.sh" {n++; h=$1} END {if(n!=1 || length(h)!=64 || h~/[^0-9a-f]/) exit 1; print h}' "$work/SHA256SUMS")
    printf '%s  %s\n' "$hash" "$work/install-linux-from-github.sh" | sha256sum -c -
    sudo sh "$work/install-linux-from-github.sh" "$version" "$expected_key_sha256"
)
```

若当前已是 root，可去掉 `sudo`。普通用户执行时，临时目录归用户且不可由其他用户改写；需保持下载目录可信，不能让其他用户替换已核验的脚本。也可先通过可信文件传输把已经核验的完整归档放到主机，再解压运行包内安装器。

下载器自动选择 x86_64 或 ARM64，从同一固定版本标签下载公钥、签名清单与对应压缩包。使用系统 CA 验证 HTTPS，限制大小与时间；核对公钥指纹、清单签名和目标归档 SHA-256 后，逐项核对归档成员与类型，拒绝目录穿越、重复成员、链接和额外文件，随后才解压并执行安装器。不采用 `curl | sh`。

## 安装后的配置与运行

默认安装路径：

- `/usr/local/bin/cowplay`：指向当前受管理版本的稳定入口。
- `/usr/local/lib/cowplay/releases/<版本>/`：版本文件，升级后保留旧程序材料以便审查。
- `/usr/local/share/cowplay`：当前版本的配置示例、说明与许可证。
- `/etc/cowplay/config.json`：用户真实配置，安装器不创建、不覆盖。
- `/var/lib/cowplay`：默认私有状态，安装器不替换或删除。
- `/var/lib/cowplay-installer`：0700 的事务记录、锁和升级前私有备份。

首次安装有 systemd 时只放置 unit 并执行 daemon-reload，不启用、不启动服务。没有正在运行的 systemd 时只安装程序文件与材料。配置时必须显式使用 `bluetooth.kind=system`；省略该字段会沿用 F50 默认后端。参照包内配置示例准备合法身份、控制器地址、热点信息、HTTPS 文件及独立管理令牌，保护配置／私钥文件为 0600、私有目录为 0700。准备完成后管理员显式启动，或选择启用开机自启。

新建的默认 `/var/lib/cowplay` 由安装器初始化首次选择状态。使用全新自定义状态目录时，启动前显式运行 `sudo cowplay init-access DIR`；它拒绝覆盖已有访问码文件。状态读取失败不会重新开放首次设置，不能用此命令修复未知或损坏的旧状态。

首次打开 CowPlay 后，可设置两次相同的六位访问码，也可明确选择不设置。选择随接收端保存，重启和升级不变。启用后，新浏览器及免码进入过的浏览器需验证一次；以前验证过的浏览器分别记住。不设置时，能打开地址的设备可直接进入。二维码只打开页面。系统蓝牙与独立管理配置见下文。

## 系统蓝牙与私有配置

完整配置示例：`packaging/config.linux.example.json`；保留原 `packaging/config.example.json` 用于 F50。Linux 平台部分如下，其余 receiver 字段仍必须提供合法持有的配件身份、真实控制器地址、已有热点和可验证的 HTTPS 证书：

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

可选管理令牌是 **32 个随机字节的 64 位十六进制文本**，与六位投屏令牌分离。按配置示例启用时，须先在目标主机创建该文件，例如由用户本机执行 `umask 077; openssl rand -hex 32 > /var/lib/cowplay/management-token`。文件须由运行服务的用户持有且无组／其他用户权限；读取使用同一 O_NOFOLLOW 文件描述符校验和限长读取。错误配置在任何蓝牙动作前失败。不需要该管理能力时省略 management_token_file。

通用管理页提供服务／连接状态、画面上限及自动降帧、访问码读取／设置／重置／停用、HTTPS 证书成对验证和热切换；手机控制台继续提供手机配对、切换、屏幕偏好、诊断与日志。管理 POST 必须同时满足独立 Bearer 管理令牌和既有 Origin 校验，普通浏览器 cookie 或投屏令牌不授予维护权限，令牌仅保存在当前页面内存，不进 URL、localStorage 或 sessionStorage。HTTP 端口迁移、系统服务控制、恢复出厂未暴露给管理 HTTP；热点、路由、启动、升级由主机工具负责。DDNS 和其他维护仍可使用同一私有控制 socket；system 的 network 迁移命令会明确拒绝 OpenWrt 专用操作。

配置完整后可由管理员执行：

```sh
cowplay probe
cowplay serve /etc/cowplay/config.json
cowplay ctl status /var/lib/cowplay/control.sock
cowplay ctl reload /var/lib/cowplay/control.sock
# 可选 Tesla 授权：第三参数使其与服务使用同一 BlueZ 后端与适配器
cowplay tesla-pair /var/lib/cowplay/tesla-ble.json /etc/cowplay/config.json
```

`packaging/cowplay.service` 是可选 systemd 模板，不自动安装或启用，不配置自动重启。它等待 network-online.target 并要求现有 bluetooth.service；主机的 wait-online／AP 配置必须覆盖所选接口与地址，不能把 network-online 本身当作热点已经就绪的证据。模板使用 `/usr/local/bin/cowplay` 与上述配置路径。F50 继续使用原 procd/APK 生命周期。

## 升级与失败恢复

升级使用相同的已验证下载入口，指定新版本即可。新包通过 `UPGRADE_FROM` 声明可接受的旧版本；同版本且内容一致时不重复停服。拒绝降级、未经声明的旧状态升级、未知的手工安装和同版本不同内容。

安装器持独占锁，先暂存并检查空间／架构，再停止原来运行的受管理服务；除 `systemctl stop` 成功外，还须看到 inactive、MainPID 为 0 且旧 PID 已退出。之后备份默认 `/etc/cowplay` 与 `/var/lib/cowplay`，切换当前程序链接，重载受管理 unit，只恢复原来的运行状态。不会调用 enable/disable，因此保留原自启选择。已停止服务保持停止。

安装器保留真实配置、配对、证书和身份，不生成或替换它们。引用在默认目录之外的私有文件须由管理员另行备份。自定义 unit 或 drop-in 不会被覆盖，自动管理模式会拒绝；可审查并停止自定义服务后显式使用 `--no-systemd` 安装程序，再由管理员维护自己的服务入口。`DESTDIR=/绝对暂存根` 用于离线文件安装，绝不操作宿主服务。

切换前失败且旧服务已安全停止时可请求恢复旧服务；停服前就会写入并落盘 `recovery-required`。断电、强制退出或切换后失败均不会盲目重跑或自动降级，事务记录、旧版本和备份保留；完整成功后才清除恢复门禁。管理员应核对当前链接、systemd 日志、版本和状态兼容性，完成恢复后才解除该标记。服务 active 只代表进程状态，不等于已经完成 CarPlay 会话、媒体、触控或音频验收。

## 自动构建和验证入口

GitHub 工作流为 Linux x86_64、ARM64 设置独立原生 runner 矩阵。两者构建嵌入前端、静态 Rust 程序，运行 Rust 测试、安装器测试和真实本地 TLS 下载验签测试，再上传各自 tar.gz。OpenWrt 双 APK 复用 ARM64 job 中已构建的静态程序。发布阶段等待全部构建与测试成功，使用专用发布密钥签名所有附件；先建立 draft 并上传全部白名单资产，完成后才公开 Release。PR／构建阶段没有生产私钥。

签名 Environment、Secret、公钥指纹变量以及许可公开审查沿用 [发布说明](RELEASES.md)。构建结果以对应提交的 GitHub Actions 日志为准；构建通过不代表已经发布生产签名附件，实际主机或 F50 安装仍须管理员授权。

构建与测试入口：

```sh
# 使用匹配架构的原生 musl Linux 构建环境；先构建 web/dist。
cargo build --locked --release
make linux-tar LINUX_ARCH=x86_64 LINUX_BINARY=target/release/cowplay
# ARM64 主机改用 LINUX_ARCH=arm64。
python3 scripts/test-linux-install.py
python3 scripts/test-linux-download.py
# 原生 APK v3 环境，验证跨平台附件签名与默认公钥导入安装。
python3 scripts/test-release-artifacts.py
```

下载测试只将 GitHub 域名在测试包装器中映射到本机 TLS 夹具，使用临时测试证书和真实 curl/OpenSSL。它证明校验及安装前拒绝路径，不证明尚未公开的真实 GitHub Release 下载或真实 Linux／树莓派硬件功能。
