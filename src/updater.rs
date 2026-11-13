use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::AsyncReadExt;

type Result<T> = std::result::Result<T, String>;
const MAX_PACKAGE: u64 = 128 * 1024 * 1024;
const MAX_JSON: usize = 1024 * 1024;
const MAX_RELEASE_LIST: usize = 8 * 1024 * 1024;
const NAMES: [&str; 2] = ["cowplay", "luci-app-cowplay"];
#[derive(Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Channel {
    #[default]
    Stable,
    Dev,
}
#[derive(Clone)]
struct Paths {
    root: PathBuf,
}
impl Paths {
    fn live() -> Self {
        Self { root: "/".into() }
    }
    fn at(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
    fn jobs(&self) -> PathBuf {
        self.at("etc/cowplay/updates")
    }
    fn job(&self, id: &str) -> Result<PathBuf> {
        if id.len() != 32 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err("无效更新任务编号".into());
        }
        Ok(self.jobs().join(id))
    }
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Source {
    repository: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Package {
    name: String,
    arch: String,
    file: String,
    size: u64,
    sha256: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    repository: String,
    tag: String,
    version: String,
    platform: String,
    openwrt_releases: Vec<String>,
    arch: String,
    upgrade_from: Vec<String>,
    packages: Vec<Package>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Job {
    id: String,
    phase: String,
    message: String,
    created: u64,
    queued: u64,
    source: Source,
    #[serde(default)]
    channel: Channel,
    tag: String,
    manifest: Option<Manifest>,
    installed: BTreeMap<String, String>,
    backup: Option<String>,
    was_running: bool,
    was_enabled: bool,
    stopped: bool,
    applying: bool,
    old_pid: Option<u32>,
    cache_only: bool,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|_| format!("无法读取 {}", path.display()))?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("文件超过大小限制".into());
    }
    Ok(bytes)
}
fn private_dir(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| e.to_string())?;
    let m = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !m.is_dir() || m.permissions().mode() & 0o077 != 0 {
        return Err("更新目录必须为私有目录（0700）".into());
    }
    Ok(())
}
fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    f50_server::settings::write_private(path, bytes).map_err(|e| e.to_string())
}
fn save(p: &Paths, j: &Job) -> Result<()> {
    write(
        &p.job(&j.id)?.join("job.json"),
        &serde_json::to_vec(j).map_err(|e| e.to_string())?,
    )
}
fn load(p: &Paths, id: &str) -> Result<Job> {
    serde_json::from_slice(&read(&p.job(id)?.join("job.json"), MAX_JSON)?)
        .map_err(|_| "更新任务记录损坏".into())
}
struct LockGuard(fs::File);
impl Drop for LockGuard {
    fn drop(&mut self) {
        // A concurrent fork may retain this open file description until exec.
        // End our operation's lock now instead of waiting for every copy to close.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN); }
    }
}
fn lock(path: &Path) -> Result<LockGuard> {
    let f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("另一更新或端口任务正在执行".into());
    }
    Ok(LockGuard(f))
}
fn repository(s: &str) -> bool {
    let parts: Vec<_> = s.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|s| {
            !s.is_empty()
                && *s != "."
                && *s != ".."
                && s.len() <= 100
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        })
}
fn version(s: &str) -> Result<[u32; 4]> {
    let (base, rev) = s.split_once("-r").ok_or("版本必须为 X.Y.Z-rN")?;
    let parts: Vec<_> = base.split('.').chain(std::iter::once(rev)).collect();
    if parts.len() != 4
        || parts.iter().any(|s| {
            s.is_empty()
                || s.len() > 9
                || !s.bytes().all(|c| c.is_ascii_digit())
                || s.len() > 1 && s.starts_with('0')
        })
    {
        return Err("版本必须为 X.Y.Z-rN".into());
    }
    let mut out = [0; 4];
    for (i, s) in parts.iter().enumerate() {
        out[i] = s.parse().map_err(|_| "版本数字无效")?;
    }
    Ok(out)
}
fn release_tag(package_version: &str) -> String {
    format!("v{}", package_version.strip_suffix("-r0").unwrap_or(package_version))
}
fn tag_version(tag: &str) -> Result<[u32; 4]> {
    let value = tag.strip_prefix('v').ok_or("发布标签必须为 vX.Y.Z 或 vX.Y.Z-rN")?;
    let package = if value.contains("-r") { value.to_owned() } else { format!("{value}-r0") };
    let parsed = version(&package)?;
    if release_tag(&package) != tag { return Err("发布标签格式不规范".into()); }
    Ok(parsed)
}
fn source(p: &Paths) -> Result<Source> {
    let config = p.at("etc/cowplay/releases.json");
    let m = fs::symlink_metadata(&config)
        .map_err(|_| "尚未配置可信发布来源；请管理员设置 releases.json 和 release-key.pem")?;
    if !m.is_file()
        || m.permissions().mode() & 0o022 != 0
        || p.root == Path::new("/") && m.uid() != 0
    {
        return Err("发布来源配置不可被非管理员修改".into());
    }
    let s: Source =
        serde_json::from_slice(&read(&config, 4096)?).map_err(|_| "发布来源配置无效")?;
    if !repository(&s.repository) {
        return Err("仓库必须是 GitHub OWNER/REPO".into());
    }
    let key = fs::symlink_metadata(p.at("etc/cowplay/release-key.pem"))
        .map_err(|_| "缺少预置信任的发布公钥")?;
    if !key.is_file()
        || key.permissions().mode() & 0o022 != 0
        || p.root == Path::new("/") && key.uid() != 0
    {
        return Err("发布公钥不可被非管理员修改".into());
    }
    Ok(s)
}
fn public(j: &Job) -> Value {
    json!({"id":j.id,"phase":j.phase,"message":j.message,"created":j.created,"repository":j.source.repository,
        "channel":j.channel,"tag":j.tag,"version":j.manifest.as_ref().map(|m| &m.version),"arch":j.manifest.as_ref().map(|m| &m.arch),
        "installed":j.installed,"backup":j.backup,"service_was_running":j.was_running,
        "packages":j.manifest.as_ref().map(|m| &m.packages),"recovery_required":j.applying && j.phase!="complete"})
}
async fn run(p: &Paths, tool: &str, args: &[String], seconds: u64) -> Result<(bool, Vec<u8>)> {
    let mut command = tokio::process::Command::new(p.at(tool));
    command.as_std_mut().process_group(0);
    let mut child = command
        .args(args)
        .env_remove("IPKG_INSTROOT")
        .env_remove("IPKG_NO_SCRIPT")
        .env_remove("APK_FLAGS")
        .env_remove("APK_CONFIG")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| format!("无法启动 {tool}"))?;
    let pid = child.id();
    let mut stdout = child.stdout.take().ok_or("无法读取命令结果")?;
    let result = tokio::time::timeout(Duration::from_secs(seconds), async {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take((MAX_JSON + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| e.to_string())?;
        if bytes.len() > MAX_JSON {
            return Err("命令输出超过限制".to_owned());
        }
        let status = child.wait().await.map_err(|e| e.to_string())?;
        Ok((status.success(), bytes))
    })
    .await
    .unwrap_or_else(|_| Err(format!("{tool} 执行超时；请检查任务状态")));
    if result.is_err() {
        if let Some(pid) = pid {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result
}
fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).into()).collect()
}
async fn required(p: &Paths, tool: &str, args: &[String], seconds: u64) -> Result<Vec<u8>> {
    let (ok, bytes) = run(p, tool, args, seconds).await?;
    if !ok {
        return Err(format!("{tool} 操作失败；请在设备本地检查系统日志"));
    }
    Ok(bytes)
}
async fn installed(p: &Paths) -> Result<BTreeMap<String, String>> {
    let bytes = required(
        p,
        "usr/bin/apk",
        &strings(&[
            "--no-network",
            "query",
            "--installed",
            "--format",
            "json",
            "--fields",
            "name,version",
            "cowplay",
            "luci-app-cowplay",
        ]),
        15,
    )
    .await?;
    let rows: Vec<Value> =
        serde_json::from_slice(&bytes).map_err(|_| "无法识别 APK v3 已安装版本信息")?;
    let mut out = BTreeMap::new();
    for row in rows {
        let name = row["name"].as_str().ok_or("APK 包名缺失")?;
        let v = row["version"].as_str().ok_or("APK 版本缺失")?;
        if !NAMES.contains(&name) || out.insert(name.into(), v.into()).is_some() {
            return Err("APK 版本查询结果异常".into());
        }
        version(v)?;
    }
    Ok(out)
}
fn validate(
    m: &Manifest,
    s: &Source,
    tag: &str,
    arch: &str,
    release: &str,
    current: &BTreeMap<String, String>,
) -> Result<()> {
    let target = version(&m.version)?;
    if m.schema != 1
        || m.repository != s.repository
        || m.tag != tag
        || tag != release_tag(&m.version)
        || m.platform != "openwrt-apk"
        || m.arch != arch
        || !m.openwrt_releases.iter().any(|v| v == release)
        || m.packages.len() != 2
    {
        return Err("发布清单的来源、版本、系统或架构不匹配".into());
    }
    for (i, name) in NAMES.iter().enumerate() {
        let pkg = &m.packages[i];
        let expected_arch = if i == 0 { arch } else { "noarch" };
        if pkg.name != *name
            || pkg.arch != expected_arch
            || pkg.file != format!("{name}-{}-{expected_arch}.apk", m.version)
            || pkg.size == 0
            || pkg.size > MAX_PACKAGE
            || pkg.sha256.len() != 64
            || !pkg
                .sha256
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err("发布清单的软件包列表、大小或哈希无效".into());
        }
    }
    for v in &m.upgrade_from {
        version(v)?;
    }
    for old in current.values() {
        if version(old)? > target {
            return Err("拒绝降级；请单独审查旧程序与状态兼容性".into());
        }
        if old != &m.version && !m.upgrade_from.contains(old) {
            return Err("发布清单未声明兼容当前版本；请先执行受支持的中间升级".into());
        }
    }
    Ok(())
}
fn platform(p: &Paths) -> Result<(String, String)> {
    let arch = String::from_utf8(read(&p.at("etc/apk/arch"), 256)?)
        .map_err(|_| "架构编码无效")?
        .trim()
        .to_owned();
    if arch != "aarch64_generic" {
        return Err("此更新通道仅支持 OpenWrt aarch64_generic APK v3".into());
    }
    let text = String::from_utf8(read(&p.at("etc/openwrt_release"), 4096)?)
        .map_err(|_| "OpenWrt 版本编码无效")?;
    let release = text
        .lines()
        .find_map(|l| l.strip_prefix("DISTRIB_RELEASE="))
        .ok_or("无法识别 OpenWrt 版本")?
        .trim_matches(['\'', '"'])
        .to_owned();
    Ok((arch, release))
}
fn host_allowed(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && matches!(
            url.host_str(),
            Some(
                "api.github.com"
                    | "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
            )
        )
}
async fn client() -> Result<reqwest::Client> {
    use rustls_pki_types::{CertificateDer, pem::PemObject};
    let mut roots = rustls::RootCertStore::empty();
    for file in [
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/ssl/cert.pem",
        "/etc/ssl/certs/ca-bundle.crt",
    ] {
        if let Ok(certs) = CertificateDer::pem_file_iter(file) {
            for cert in certs.flatten() {
                let _ = roots.add(cert);
            }
        }
    }
    if roots.is_empty() {
        return Err("缺少系统 CA；请安装 ca-bundle".into());
    }
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| e.to_string())?
    .with_root_certificates(roots)
    .with_no_client_auth();
    reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .user_agent("CowPlay-release-updater")
        .use_preconfigured_tls(tls)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(180))
        .redirect(reqwest::redirect::Policy::custom(|a| {
            if a.previous().len() < 5 && host_allowed(a.url()) {
                a.follow()
            } else {
                a.error("发布下载重定向被拒绝")
            }
        }))
        .build()
        .map_err(|_| "无法初始化 HTTPS 客户端".into())
}
async fn download(http: &reqwest::Client, url: &str, path: &Path, limit: u64) -> Result<()> {
    let parsed = reqwest::Url::parse(url).map_err(|_| "发布下载 URL 无效")?;
    if !host_allowed(&parsed) {
        return Err("拒绝非 GitHub HTTPS 下载地址".into());
    }
    let mut response = http
        .get(parsed)
        .send()
        .await
        .map_err(|e| format!("GitHub 请求失败：{}", e.without_url()))?;
    if !response.status().is_success() {
        return Err(format!(
            "GitHub HTTP {}；请检查网络、公开仓库、发布状态或 API 限流",
            response.status().as_u16()
        ));
    }
    if response.content_length().is_some_and(|s| s > limit) {
        return Err("下载文件过大".into());
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut size = 0u64;
    while let Some(bytes) = response
        .chunk()
        .await
        .map_err(|_| "发布下载中断".to_owned())?
    {
        size += bytes.len() as u64;
        if size > limit {
            return Err("下载文件超过限制".into());
        }
        file.write_all(&bytes)
            .map_err(|_| "下载写入失败；请检查存储空间")?;
    }
    file.sync_all().map_err(|e| e.to_string())
}
async fn verify_manifest(p: &Paths, dir: &Path) -> Result<Manifest> {
    required(
        p,
        "usr/bin/openssl",
        &vec![
            "dgst".into(),
            "-sha256".into(),
            "-verify".into(),
            p.at("etc/cowplay/release-key.pem").display().to_string(),
            "-signature".into(),
            dir.join("cowplay-release.sig").display().to_string(),
            dir.join("cowplay-release.json").display().to_string(),
        ],
        15,
    )
    .await
    .map_err(|_| "发布清单签名验证失败；尚未操作服务或安装软件包".to_owned())?;
    serde_json::from_slice(&read(&dir.join("cowplay-release.json"), MAX_JSON)?)
        .map_err(|_| "发布清单 JSON 格式无效".into())
}
fn select_release<'a>(info: &'a Value, tag: &str, channel: Channel) -> Result<&'a Value> {
    let matches = |release: &&Value| {
        release["draft"] == false && release["prerelease"] == (channel == Channel::Dev)
    };
    if tag.is_empty() && channel == Channel::Dev {
        return info.as_array().ok_or("GitHub 返回的不是版本列表")?
            .iter().filter(matches)
            .filter_map(|release| tag_version(release["tag_name"].as_str()?).ok().map(|version| (version, release)))
            .max_by_key(|(version, _)| *version).map(|(_, release)| release)
            .ok_or_else(|| "暂无可用开发版，请检查预发布版本列表".into());
    }
    if !matches(&info) {
        return Err("版本不属于所选渠道，或仍是草稿；正式版与开发版不能混用".into());
    }
    let actual = info["tag_name"].as_str().ok_or("GitHub 发布缺少标签")?;
    tag_version(actual)?;
    if !tag.is_empty() && actual != tag {
        return Err("GitHub 返回的标签与所选版本不符".into());
    }
    Ok(info)
}
async fn check(p: &Paths, j: &mut Job) -> Result<()> {
    let (arch, release) = platform(p)?;
    j.installed = installed(p).await?;
    let dir = p.job(&j.id)?;
    let http = client().await?;
    let endpoint = if !j.tag.is_empty() {
        format!("releases/tags/{}", j.tag)
    } else if j.channel == Channel::Dev {
        "releases?per_page=100".to_owned()
    } else {
        "releases/latest".to_owned()
    };
    download(
        &http,
        &format!(
            "https://api.github.com/repos/{}/{endpoint}",
            j.source.repository
        ),
        &dir.join("github.json"),
        if j.tag.is_empty() && j.channel == Channel::Dev { MAX_RELEASE_LIST } else { MAX_JSON } as u64,
    )
    .await?;
    let release_info: Value = serde_json::from_slice(&read(&dir.join("github.json"), MAX_RELEASE_LIST)?)
        .map_err(|_| "GitHub 返回的数据不是发布信息")?;
    let release_info = select_release(&release_info, &j.tag, j.channel)?;
    let tag = release_info["tag_name"]
        .as_str()
        .ok_or("GitHub 发布缺少标签")?;
    tag_version(tag)?;
    if !j.tag.is_empty() && j.tag != tag {
        return Err("GitHub 返回的标签与所选版本不符".into());
    }
    j.tag = tag.into();
    let assets = release_info["assets"]
        .as_array()
        .ok_or("GitHub 发布没有附件")?;
    for name in ["cowplay-release.json", "cowplay-release.sig"] {
        let matches: Vec<_> = assets
            .iter()
            .filter(|a| a["name"] == name && a["state"] == "uploaded")
            .collect();
        if matches.len() != 1 {
            return Err(format!("发布缺少唯一的 {name}"));
        }
        let expected = format!(
            "https://github.com/{}/releases/download/{}/{name}",
            j.source.repository, j.tag
        );
        if matches[0]["browser_download_url"] != expected {
            return Err("发布附件地址与可信仓库不符".into());
        }
        download(
            &http,
            &expected,
            &dir.join(name),
            if name.ends_with(".sig") {
                16384
            } else {
                MAX_JSON as u64
            },
        )
        .await?;
    }
    let m = verify_manifest(p, &dir).await?;
    validate(&m, &j.source, &j.tag, &arch, &release, &j.installed)?;
    j.manifest = Some(m);
    j.phase = "ready".into();
    j.message = "发布清单签名已验证；安装前仍须下载并验证两个 APK".into();
    if j.installed.len() == 2
        && j.installed
            .values()
            .all(|v| Some(v) == j.manifest.as_ref().map(|m| &m.version))
    {
        j.phase = "current".into();
        j.message = "核心与 LuCI 已是所选版本".into();
    }
    save(p, j)
}
fn file_hash(path: &Path, expected: &Package) -> Result<()> {
    let mut file = fs::File::open(path).map_err(|_| "软件包下载缺失")?;
    let mut size = 0;
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = file.read(&mut buf).map_err(|_| "软件包读取失败")?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > expected.size {
            return Err("软件包大小不符".into());
        }
        hash.update(&buf[..n]);
    }
    if size != expected.size || format!("{:x}", hash.finalize()) != expected.sha256 {
        return Err("软件包 SHA256 或大小不符".into());
    }
    Ok(())
}
fn apk_args(dir: &Path) -> Vec<String> {
    vec![
        "--no-network".into(),
        "--no-cache".into(),
        "--repositories-file".into(),
        "/dev/null".into(),
        "--keys-dir".into(),
        dir.join("keys").display().to_string(),
    ]
}
async fn verify_packages(p: &Paths, dir: &Path, m: &Manifest) -> Result<u64> {
    let mut installed_size = 0u64;
    private_dir(&dir.join("keys"))?;
    write(
        &dir.join("keys/release.pem"),
        &read(&p.at("etc/cowplay/release-key.pem"), 16384)?,
    )?;
    for pkg in &m.packages {
        let file = dir.join(&pkg.file);
        file_hash(&file, pkg)?;
        let mut args = apk_args(dir);
        args.extend(["verify".into(), file.display().to_string()]);
        required(p, "usr/bin/apk", &args, 30)
            .await
            .map_err(|_| "APK 签名或完整性验证失败".to_owned())?;
        let mut args = apk_args(dir);
        args.extend([
            "adbdump".into(),
            "--format".into(),
            "json".into(),
            file.display().to_string(),
        ]);
        let bytes = required(p, "usr/bin/apk", &args, 30).await?;
        let data: Value = serde_json::from_slice(&bytes).map_err(|_| "无法读取 APK v3 元数据")?;
        let info = &data["info"];
        installed_size = installed_size
            .checked_add(info["installed-size"].as_u64().ok_or("APK 缺少安装体积")?)
            .ok_or("APK 安装体积溢出")?;
        if info["name"] != pkg.name || info["version"] != m.version || info["arch"] != pkg.arch {
            return Err("APK 内嵌包名、版本或架构与签名清单不符".into());
        }
    }
    Ok(installed_size)
}
async fn service(p: &Paths, action: &str) -> Result<bool> {
    Ok(run(p, "etc/init.d/cowplay", &strings(&[action]), 30)
        .await?
        .0)
}
async fn stopped(p: &Paths, pid: Option<u32>) -> Result<()> {
    for _ in 0..20 {
        if (!p.at("etc/init.d/cowplay").exists() || !service(p, "running").await?)
            && !p.at("sys/class/bluetooth/hci0").exists()
            && pid.is_none_or(|pid| !p.at(&format!("proc/{pid}")).exists())
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err("CowPlay 或 HCI 尚未释放；未执行安装，请本地检查进程和蓝牙".into())
}
async fn install(p: &Paths, j: &mut Job) -> Result<()> {
    if source(p)? != j.source {
        return Err("可信发布来源已变化，请重新检查版本".into());
    }
    if now().saturating_sub(j.created) > 900 {
        return Err("版本检查已超过 15 分钟，请重新检查".into());
    }
    let dir = p.job(&j.id)?;
    let m = verify_manifest(p, &dir).await?;
    let (arch, release) = platform(p)?;
    let current = installed(p).await?;
    if current != j.installed {
        return Err("设备已安装版本发生变化，请重新检查".into());
    }
    validate(&m, &j.source, &j.tag, &arch, &release, &current)?;
    j.phase = "downloading".into();
    j.message = "正在下载核心和 LuCI；服务保持当前状态".into();
    save(p, j)?;
    space(
        p,
        m.packages
            .iter()
            .map(|p| p.size)
            .sum::<u64>()
            .saturating_mul(2)
            + 16 * 1024 * 1024,
    )?;
    let http = client().await?;
    download(&http, &format!("https://api.github.com/repos/{}/releases/tags/{}", j.source.repository, j.tag),
        &dir.join("github-install.json"), MAX_JSON as u64).await?;
    let release_info: Value = serde_json::from_slice(&read(&dir.join("github-install.json"), MAX_JSON)?)
        .map_err(|_| "GitHub 返回的数据不是发布信息")?;
    select_release(&release_info, &j.tag, j.channel)?;
    for pkg in &m.packages {
        download(
            &http,
            &format!(
                "https://github.com/{}/releases/download/{}/{}",
                j.source.repository, j.tag, pkg.file
            ),
            &dir.join(&pkg.file),
            pkg.size,
        )
        .await?;
    }
    if j.cache_only {
        verify_packages(p, &dir, &m).await?;
        if installed(p).await? != j.installed {
            return Err("准备缓存期间设备版本已变化，请重新检查".into());
        }
        if j.installed.len() != 2 || j.installed.values().any(|v| v != &m.version) {
            return Err("系统升级缓存必须与两个已安装包版本一致；请先完成应用升级".into());
        }
        j.was_enabled = service(p, "enabled").await?;
        persist_restore(p, j, &m)?;
        j.phase = "complete".into();
        j.message =
            "已准备系统升级恢复缓存；仅保留设置且固件版本声明兼容时自动恢复，sysupgrade -n 不保留"
                .into();
        save(p, j)
    } else {
        apply_verified(p, j, &m).await
    }
}
async fn apply_verified(p: &Paths, j: &mut Job, m: &Manifest) -> Result<()> {
    let dir = p.job(&j.id)?;
    let installed_size = verify_packages(p, &dir, m).await?;
    let state_size = tree_size(&p.at("etc/cowplay/state"))?;
    space(
        p,
        installed_size
            .saturating_add(state_size.saturating_mul(2))
            .saturating_add(m.packages.iter().map(|pkg| pkg.size).sum::<u64>())
            .saturating_add(16 * 1024 * 1024),
    )?;
    let mut args = apk_args(&dir);
    args.extend(["add".into(), "--upgrade".into()]);
    args.extend(
        m.packages
            .iter()
            .map(|pkg| dir.join(&pkg.file).display().to_string()),
    );
    args.extend(NAMES.iter().map(|name| format!("{name}={}", m.version)));
    let mut simulation = args.clone();
    simulation.push("--simulate".into());
    required(p, "usr/bin/apk", &simulation, 60)
        .await
        .map_err(|_| "离线依赖检查失败；请管理员先安装缺少的系统依赖".to_owned())?;
    if installed(p).await? != j.installed {
        return Err("下载期间设备版本已变化，请重新检查".into());
    }
    if current_core(j) && !p.at("etc/init.d/cowplay").exists() {
        return Err("已安装核心但启动脚本缺失，请先修复安装".into());
    }
    j.was_running = p.at("etc/init.d/cowplay").exists() && service(p, "running").await?;
    j.was_enabled = p.at("etc/init.d/cowplay").exists() && service(p, "enabled").await?;
    j.phase = "stopping".into();
    j.message = "软件包验证完成，正在停止 CowPlay；画面和声音将中断".into();
    save(p, j)?;
    if j.was_running {
        let bytes = required(
            p,
            "bin/ubus",
            &strings(&["call", "service", "list", "{\"name\":\"cowplay\"}"]),
            10,
        )
        .await?;
        let data: Value = serde_json::from_slice(&bytes).map_err(|_| "无法识别旧 CowPlay 进程")?;
        let instances = data["cowplay"]["instances"]
            .as_object()
            .ok_or("找不到旧 CowPlay 进程")?;
        let pids: Vec<_> = instances
            .values()
            .filter_map(|v| v["pid"].as_u64())
            .collect();
        if pids.len() != 1 || pids[0] <= 1 || pids[0] > u32::MAX as u64 {
            return Err("旧 CowPlay PID 不明确；未停止或安装".into());
        }
        j.old_pid = Some(pids[0] as u32);
        j.stopped = true;
        save(p, j)?;
        if !service(p, "stop").await? {
            return Err("停止 CowPlay 失败；未执行安装".into());
        }
    }
    stopped(p, j.old_pid).await?;
    let backup = dir.join("backup.tgz");
    let mut tar = vec![
        "-czf".into(),
        backup.display().to_string(),
        "-C".into(),
        p.root.display().to_string(),
    ];
    for path in ["etc/cowplay/state", "etc/config/cowplay", "etc/apk/world"] {
        if p.at(path).exists() {
            tar.push(path.into());
        }
    }
    if tar.len() == 4 {
        return Err("没有可备份的设备配置，停止安装".into());
    }
    j.phase = "backing_up".into();
    j.message = "正在设备本地备份配置和私有状态".into();
    save(p, j)?;
    required(p, "bin/tar", &tar, 120)
        .await
        .map_err(|_| "私有状态备份失败；未执行安装，请检查存储空间".to_owned())?;
    fs::set_permissions(&backup, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    fs::File::open(&backup)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    j.backup = Some(backup.display().to_string());
    if installed(p).await? != j.installed {
        return Err("备份期间设备版本已变化；未执行安装，请重新检查".into());
    }
    private_dir(&p.at("etc/cowplay/firmware-restore"))?;
    write(
        &p.at("etc/cowplay/firmware-restore/blocked"),
        j.id.as_bytes(),
    )?;
    j.applying = true;
    j.phase = "installing".into();
    j.message = "正在安装两个软件包；请勿断电或操作服务".into();
    save(p, j)?;
    required(p, "usr/bin/apk", &args, 180).await.map_err(|_| {
        "APK 安装未完成；服务保持停止，请检查软件包状态后恢复，禁止盲目降级".to_owned()
    })?;
    let after = installed(p).await?;
    if after.len() != 2 || after.values().any(|v| v != &m.version) {
        return Err("安装后核心与 LuCI 版本不一致；需要本地修复".into());
    }
    let enabled = service(p, "enabled").await?;
    if current_core(j) && enabled != j.was_enabled || !current_core(j) && !enabled {
        return Err("安装后的开机自启与预期不一致；需要本地检查".into());
    }
    if j.was_running {
        j.phase = "starting".into();
        j.message = "软件包已安装，正在恢复原运行状态".into();
        save(p, j)?;
        if !service(p, "start").await? {
            return Err("新版服务启动失败；配置及升级前备份保留".into());
        }
        let mut running = false;
        for _ in 0..15 {
            if service(p, "running").await? {
                running = true;
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        if !running {
            return Err("新版服务未恢复运行；请检查日志，配置及备份保留".into());
        }
        let health = required(p, "usr/bin/cowplay", &strings(&["ctl", "status"]), 15).await?;
        if serde_json::from_slice::<Value>(&health)
            .ok()
            .and_then(|v| v["pid"].as_u64())
            .is_none()
        {
            return Err("新版管理接口尚未就绪；请检查日志".into());
        }
    }
    persist_restore(p, j, m)?;
    j.phase = "complete".into();
    j.message = if j.was_running {
        "核心与 LuCI 已更新，原运行状态已恢复；请刷新页面并重新连接验证"
    } else if current_core(j) {
        "核心与 LuCI 已更新；服务保持停止，自启选择保留"
    } else {
        "核心与 LuCI 已安装；服务保持停止，首次核心安装已启用下次开机自启"
    }
    .into();
    save(p, j)
}
fn persist_restore(p: &Paths, j: &Job, m: &Manifest) -> Result<()> {
    let cache = p.at("etc/cowplay/firmware-restore");
    private_dir(&cache)?;
    let slot = cache.join(&j.id);
    private_dir(&slot)?;
    let dir = p.job(&j.id)?;
    for file in ["cowplay-release.json", "cowplay-release.sig"]
        .into_iter()
        .chain(m.packages.iter().map(|pkg| pkg.file.as_str()))
    {
        let target = slot.join(file);
        fs::copy(dir.join(file), &target).map_err(|_| "无法保存系统升级恢复缓存".to_owned())?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        fs::File::open(&target)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
    }
    write(
        &slot.join("autostart"),
        if current_core(j) && !j.was_enabled {
            b"disabled"
        } else {
            b"enabled"
        },
    )?;
    fs::File::open(&slot)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    write(&cache.join("current"), j.id.as_bytes())?;
    if cache.join("blocked").exists() {
        fs::remove_file(cache.join("blocked")).map_err(|e| e.to_string())?;
        fs::File::open(&cache)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
    }
    for entry in fs::read_dir(&cache).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.len() == 32
            && name.bytes().all(|b| b.is_ascii_hexdigit())
            && name != j.id
            && entry.file_type().map_err(|e| e.to_string())?.is_dir()
        {
            fs::remove_dir_all(entry.path()).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
fn tree_size(path: &Path) -> Result<u64> {
    let m = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.to_string()),
    };
    if !m.is_dir() {
        return Ok(m.len());
    }
    let mut sum = 0u64;
    for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
        sum = sum.saturating_add(tree_size(&entry.map_err(|e| e.to_string())?.path())?);
    }
    Ok(sum)
}
fn space(p: &Paths, needed: u64) -> Result<()> {
    let name = std::ffi::CString::new(p.jobs().as_os_str().as_encoded_bytes())
        .map_err(|_| "更新目录无效")?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(name.as_ptr(), &mut stat) } != 0 {
        return Err("无法检查更新存储空间".into());
    }
    let available = (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64);
    if available < needed {
        return Err(format!(
            "更新空间不足；至少需要 {} MiB 可用空间",
            needed.div_ceil(1024 * 1024)
        ));
    }
    Ok(())
}
fn cleanup(p: &Paths, j: &Job) -> Result<()> {
    let dir = p.job(&j.id)?;
    let _ = fs::remove_file(dir.join("github.json"));
    if !j.applying || j.phase == "complete" {
        if let Some(m) = &j.manifest {
            for pkg in &m.packages {
                let _ = fs::remove_file(dir.join(&pkg.file));
            }
        }
    }
    Ok(())
}
fn supersede(p: &Paths) -> Result<()> {
    let Ok(id) = fs::read_to_string(p.jobs().join("latest")) else {
        return Ok(());
    };
    let mut old = load(p, id.trim())?;
    if ["ready", "current"].contains(&old.phase.as_str()) {
        old.phase = "superseded".into();
        old.message = "已有新的版本检查；请使用最新检查结果".into();
        save(p, &old)?;
        let dir = p.job(&old.id)?;
        for name in ["github.json", "cowplay-release.json", "cowplay-release.sig"] {
            let _ = fs::remove_file(dir.join(name));
        }
    }
    cleanup(p, &old)
}
fn current_core(j: &Job) -> bool {
    j.installed.contains_key("cowplay")
}
fn release_active(p: &Paths, j: &Job) -> Result<()> {
    if fs::read_to_string(p.jobs().join("active")).ok().as_deref() == Some(&j.id) {
        fs::remove_file(p.jobs().join("active")).map_err(|e| e.to_string())?;
        fs::File::open(p.jobs())
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn spawn(p: &Paths, j: &Job) -> Result<()> {
    let _ = p.job(&j.id)?;
    let temporary = tempfile::Builder::new()
        .prefix("cowplay-updater-")
        .tempfile()
        .map_err(|e| e.to_string())?;
    let executable = temporary.path().to_owned();
    {
        fs::copy(
            std::env::current_exe().map_err(|e| e.to_string())?,
            &executable,
        )
        .map_err(|e| e.to_string())?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    let mut command = std::process::Command::new(executable);
    command
        .args(["update-worker", &j.id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::umask(0o077);
            Ok(())
        });
    }
    let _temporary_path = temporary.into_temp_path();
    command
        .spawn()
        .map(|_| ())
        .map_err(|_| "无法启动独立更新任务".into())
}
fn network_lock(p: &Paths) -> Result<LockGuard> {
    let dir = p.at("var/run/cowplay-network");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let guard = lock(&dir.join("lock"))?;
    if dir.join("active").exists() {
        return Err("端口接管任务尚未完成，请先完成或恢复".into());
    }
    Ok(guard)
}
fn terminal(phase: &str) -> bool {
    matches!(
        phase,
        "ready" | "current" | "complete" | "failed" | "interrupted" | "acknowledged" | "superseded"
    )
}
fn status(p: &Paths) -> Result<Value> {
    let src = source(p);
    let latest = fs::read_to_string(p.jobs().join("latest")).ok();
    let Some(id) = latest else {
        return Ok(
            json!({"phase":"idle","repository":src.as_ref().ok().map(|s| &s.repository),"configuration_error":src.err()}),
        );
    };
    let mut j = load(p, id.trim())?;
    if p.jobs().join("active").exists()
        || !terminal(&j.phase) && now().saturating_sub(j.queued) > 10
    {
        if let Ok(_guard) = lock(&p.jobs().join("lock")) {
            j = load(p, id.trim())?;
            if ["ready", "current", "complete", "acknowledged", "superseded"]
                .contains(&j.phase.as_str())
                || j.phase == "failed" && !j.stopped && !j.applying
            {
                release_active(p, &j)?;
            } else if !terminal(&j.phase) && now().saturating_sub(j.queued) > 10 {
                j.phase = "interrupted".into();
                j.message = "更新进程已退出或设备重启；请检查已安装版本和服务，再解除任务锁定。备份保留，不自动降级或还原状态".into();
                save(p, &j)?;
            }
        }
    }
    let mut value = public(&j);
    value["configuration_error"] = json!(src.err());
    value["locked"] = json!(p.jobs().join("active").exists());
    Ok(value)
}
pub async fn cli(action: &str, input: &Value) -> Result<Value> {
    cli_at(&Paths::live(), action, input).await
}

/// Local administrator bootstrap using the executable's embedded release key.
/// Used by package hooks and the signature-verified helper; never exposed through rpcd.
pub fn bootstrap() -> Result<()> {
    bootstrap_at(&Paths::live(), include_bytes!("../packaging/release-key.pem"))
}
fn bootstrap_at(p: &Paths, key: &[u8]) -> Result<()> {
    let dir = p.at("etc/cowplay");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let metadata = fs::symlink_metadata(&dir).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o022 != 0
        || p.root == Path::new("/") && metadata.uid() != 0 {
        return Err("发布配置目录必须由管理员控制，不能是链接".into());
    }
    let _guard = lock(&dir.join("release-bootstrap.lock"))?;
    let config = p.at("etc/cowplay/releases.json");
    let public_key = p.at("etc/cowplay/release-key.pem");
    if fs::symlink_metadata(&config).is_ok() || fs::symlink_metadata(&public_key).is_ok() {
        let existing = source(p)?;
        if existing.repository != "helloworlde/cowplay" || read(&public_key, 16384)? != key {
            return Err("已有不同的发布来源或公钥，未替换；请管理员核对".into());
        }
        return Ok(());
    }
    write(&public_key, key)?;
    if let Err(error) = write(&config, b"{\"repository\":\"helloworlde/cowplay\"}\n") {
        let _ = fs::remove_file(&public_key);
        return Err(error);
    }
    Ok(())
}
async fn cli_at(p: &Paths, action: &str, input: &Value) -> Result<Value> {
    if action == "status" {
        let mut value = status(p)?;
        let cache = p.at("etc/cowplay/firmware-restore");
        value["system_upgrade"] = json!({"prepared":cache.join("current").exists(), "blocked":cache.join("blocked").exists(), "result":read(&cache.join("result"),4096).ok().map(|b|String::from_utf8_lossy(&b).into_owned())});
        if p.at("usr/bin/apk").exists() {
            match installed(p).await {
                Ok(current) => value["installed"] = json!(current),
                Err(e) => {
                    value["installed"] = json!({});
                    value["installed_error"] = json!(e);
                }
            }
        }
        return Ok(value);
    }
    if unsafe { libc::geteuid() } != 0 && p.root == Path::new("/") {
        return Err("更新操作需要管理员权限".into());
    }
    private_dir(&p.jobs())?;
    let _guard = lock(&p.jobs().join("lock"))?;
    if action == "acknowledge" {
        let mut j = load(p, input["id"].as_str().ok_or("缺少任务编号")?)?;
        if !["failed", "interrupted"].contains(&j.phase.as_str()) {
            return Err("任务仍在执行或无需恢复".into());
        }
        release_active(p, &j)?;
        j.phase = "acknowledged".into();
        j.message = "已确认人工检查；备份和任务记录保留，可以重新检查版本".into();
        save(p, &j)?;
        return Ok(public(&j));
    }
    let _network = network_lock(p)?;
    if p.jobs().join("active").exists() {
        return Err("已有更新任务，请先完成或检查恢复状态".into());
    }
    let mut j = match action {
        "check" => {
            let s = source(p)?;
            let channel = input.get("channel").map(|value| serde_json::from_value(value.clone()))
                .transpose().map_err(|_| "更新渠道必须为 stable 或 dev")?.unwrap_or_default();
            let tag = input["tag"].as_str().unwrap_or("");
            if !tag.is_empty() {
                tag_version(tag)?;
            }
            let mut random = [0; 16];
            fs::File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(&mut random))
                .map_err(|e| e.to_string())?;
            let id: String = random.iter().map(|b| format!("{b:02x}")).collect();
            private_dir(&p.job(&id)?)?;
            Job {
                id,
                phase: "checking".into(),
                message: "正在检查 GitHub 发布及清单签名".into(),
                created: now(),
                queued: now(),
                source: s,
                channel,
                tag: tag.into(),
                manifest: None,
                installed: BTreeMap::new(),
                backup: None,
                was_running: false,
                was_enabled: false,
                stopped: false,
                applying: false,
                old_pid: None,
                cache_only: false,
            }
        }
        "apply" | "prepare" => {
            let mut j = load(p, input["id"].as_str().ok_or("缺少任务编号")?)?;
            if j.phase != "ready" && !(action == "prepare" && j.phase == "current") {
                return Err("请先重新检查可安装版本".into());
            }
            j.cache_only = action == "prepare";
            j.queued = now();
            j.phase = "queued".into();
            j.message = "安装任务已排队".into();
            j
        }
        _ => return Err("未知更新操作".into()),
    };
    if action == "check" {
        supersede(p)?;
    }
    save(p, &j)?;
    write(&p.jobs().join("latest"), j.id.as_bytes())?;
    write(&p.jobs().join("active"), j.id.as_bytes())?;
    if let Err(e) = spawn(p, &j) {
        j.phase = "failed".into();
        j.message = e;
        save(p, &j)?;
        release_active(p, &j)?;
    }
    Ok(public(&j))
}
pub async fn worker(id: &str) -> Result<()> {
    let p = Paths::live();
    let mut guard = None;
    for _ in 0..30 {
        if let Ok(g) = lock(&p.jobs().join("lock")) {
            guard = Some(g);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let _guard = guard.ok_or("无法取得更新任务锁")?;
    let mut j = load(&p, id)?;
    if fs::read_to_string(p.jobs().join("active")).ok().as_deref() != Some(id) {
        return Err("更新任务已失效".into());
    }
    let _network = network_lock(&p)?;
    let checking = j.phase == "checking";
    let result = if checking {
        check(&p, &mut j).await
    } else if j.phase == "queued" {
        install(&p, &mut j).await
    } else {
        Err("更新任务阶段无效".into())
    };
    finish(&p, &mut j, result, checking).await
}
async fn finish(p: &Paths, j: &mut Job, result: Result<()>, checking: bool) -> Result<()> {
    if let Err(error) = result {
        if j.applying && j.phase == "starting" {
            let _ = service(p, "stop").await;
        }
        j.phase = "failed".into();
        j.message = error;
        if j.stopped && !j.applying && j.was_running {
            if stopped(p, j.old_pid).await.is_ok() && service(p, "start").await.unwrap_or(false) {
                j.message.push_str("；旧服务已请求恢复启动");
            } else {
                j.message.push_str("；旧服务未恢复，请本地检查后手动启动");
            }
        }
        save(p, j)?;
    }
    cleanup(p, j)?;
    if checking || j.phase == "complete" || !j.stopped && !j.applying {
        release_active(p, j)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest() -> Manifest {
        Manifest {
            schema: 1,
            repository: "example/cowplay".into(),
            tag: "v0.3.15".into(),
            version: "0.3.15-r0".into(),
            platform: "openwrt-apk".into(),
            openwrt_releases: vec!["25.12.6".into()],
            arch: "aarch64_generic".into(),
            upgrade_from: vec!["0.3.14-r16".into()],
            packages: NAMES
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    let arch = if i == 0 { "aarch64_generic" } else { "noarch" };
                    Package {
                        name: (*n).into(),
                        arch: arch.into(),
                        file: format!("{n}-0.3.15-r0-{arch}.apk"),
                        size: 4,
                        sha256: format!("{:x}", Sha256::digest(b"test")),
                    }
                })
                .collect(),
        }
    }
    fn job() -> Job {
        Job {
            id: "0123456789abcdef0123456789abcdef".into(),
            phase: "queued".into(),
            message: String::new(),
            created: now(),
            queued: now(),
            source: Source {
                repository: "example/cowplay".into(),
            },
            channel: Channel::Stable,
            tag: "v0.3.15".into(),
            manifest: Some(manifest()),
            installed: NAMES
                .iter()
                .map(|n| ((*n).into(), "0.3.14-r16".into()))
                .collect(),
            backup: None,
            was_running: false,
            was_enabled: false,
            stopped: false,
            applying: false,
            old_pid: None,
            cache_only: false,
        }
    }
    fn fixture() -> (tempfile::TempDir, Paths, Job) {
        let tmp = tempfile::tempdir().unwrap();
        let p = Paths {
            root: tmp.path().to_owned(),
        };
        let j = job();
        for dir in [
            "usr/bin",
            "etc/init.d",
            "etc/apk",
            "etc/config",
            "etc/cowplay/state",
            "bin",
            "proc",
        ] {
            fs::create_dir_all(p.at(dir)).unwrap();
        }
        private_dir(&p.jobs()).unwrap();
        private_dir(&p.job(&j.id).unwrap()).unwrap();
        fs::write(
            p.job(&j.id).unwrap().join("cowplay-release.json"),
            serde_json::to_vec(&j.manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            p.job(&j.id).unwrap().join("cowplay-release.sig"),
            b"synthetic-signature",
        )
        .unwrap();
        fs::write(p.at("etc/apk/arch"), "aarch64_generic\n").unwrap();
        fs::write(p.at("etc/openwrt_release"), "DISTRIB_RELEASE='25.12.6'\n").unwrap();
        write(
            &p.at("etc/cowplay/releases.json"),
            b"{\"repository\":\"example/cowplay\"}",
        )
        .unwrap();
        write(
            &p.at("etc/cowplay/release-key.pem"),
            b"synthetic-public-key",
        )
        .unwrap();
        write(
            &p.at("etc/cowplay/state/identity"),
            b"SYNTHETIC PRIVATE STATE",
        )
        .unwrap();
        write(&p.at("etc/config/cowplay"), b"config cowplay main\n").unwrap();
        fs::write(
            p.at("installed.json"),
            serde_json::to_vec(
                &NAMES
                    .iter()
                    .map(|n| json!({"name":n,"version":"0.3.14-r16"}))
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
        )
        .unwrap();
        fs::write(
            p.at("target.json"),
            serde_json::to_vec(
                &NAMES
                    .iter()
                    .map(|n| json!({"name":n,"version":"0.3.15-r0"}))
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
        )
        .unwrap();
        for pkg in &j.manifest.as_ref().unwrap().packages {
            fs::write(p.job(&j.id).unwrap().join(&pkg.file), b"test").unwrap();
            fs::write(p.at(&format!("{}.meta",pkg.file)),json!({"info":{"name":pkg.name,"version":"0.3.15-r0","arch":pkg.arch,"installed-size":4}}).to_string()).unwrap();
        }
        script(
            &p,
            "usr/bin/apk",
            r#"
for arg in "$@"; do
 case "$arg" in
 query) cat "$R/installed.json"; exit;;
 verify) echo verify >> "$R/events"; test ! -e "$R/bad_signature"; exit;;
 adbdump) for file in "$@"; do case "$file" in *.apk) cat "$R/$(basename "$file").meta"; exit;; esac; done;;
 add) case " $* " in *' --simulate '*) echo simulate >> "$R/events"; test ! -e "$R/bad_dependencies"; exit;; esac
      echo install >> "$R/events"
      test ! -e "$R/fail_install" || exit 1
      cp "$R/target.json" "$R/installed.json"
      test -e "$R/etc/init.d/cowplay" || cp "$R/init-template" "$R/etc/init.d/cowplay"
      test ! -e "$R/first_install" || touch "$R/enabled"
      exit 0;;
 esac
done
exit 1
"#,
        );
        script(
            &p,
            "etc/init.d/cowplay",
            r#"
case "$1" in
 running) test -e "$R/running";;
 enabled) test -e "$R/enabled";;
 stop) echo stop >> "$R/events"; test ! -e "$R/fail_stop" || exit 1; rm -f "$R/running";;
 start) echo start >> "$R/events"; test ! -e "$R/fail_start" || exit 1; touch "$R/running";;
esac
"#,
        );
        fs::copy(p.at("etc/init.d/cowplay"), p.at("init-template")).unwrap();
        script(&p, "usr/bin/cowplay", "printf '{\"pid\":1234}'");
        script(
            &p,
            "bin/ubus",
            r#"printf '{"cowplay":{"instances":{"main":{"pid":99999999}}}}'"#,
        );
        std::os::unix::fs::symlink(
            if Path::new("/bin/tar").exists() {
                "/bin/tar"
            } else {
                "/usr/bin/tar"
            },
            p.at("bin/tar"),
        )
        .unwrap();
        (tmp, p, j)
    }
    fn script(p: &Paths, name: &str, body: &str) {
        let text = format!("#!/bin/sh\nR='{}'\n{body}\n", p.root.display());
        fs::write(p.at(name), text).unwrap();
        fs::set_permissions(p.at(name), fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn events(p: &Paths) -> String {
        fs::read_to_string(p.at("events")).unwrap_or_default()
    }
    #[test]
    fn release_tags_are_canonical() {
        assert_eq!(release_tag("0.0.1-r0"), "v0.0.1");
        assert_eq!(release_tag("0.0.1-r1"), "v0.0.1-r1");
        assert_eq!(tag_version("v0.0.1").unwrap(), [0, 0, 1, 0]);
        assert!(tag_version("v0.0.1-r0").is_err());
        assert!(tag_version("v0.0.1/evil").is_err());
    }
    #[test]
    fn lock_release_does_not_wait_for_inherited_descriptors() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("lock");
        let guard = lock(&path).unwrap();
        // dup and fork share the same open file description and flock ownership.
        let inherited = guard.0.try_clone().unwrap();
        assert!(lock(&path).is_err(), "a live operation must remain exclusive");
        drop(guard);
        let next = lock(&path).expect("a finished operation must release its lock before child exec/exit");
        assert_eq!(unsafe { libc::flock(inherited.as_raw_fd(), libc::LOCK_UN) }, 0);
        assert!(lock(&path).is_err(), "unlocking an old child descriptor must not release the new owner");
        drop(inherited);
        assert!(lock(&path).is_err(), "an old child descriptor must not release the new owner");
        drop(next);
        assert!(lock(&path).is_ok());
    }
    #[test]
    fn bootstrap_is_idempotent_and_never_replaces_existing_trust() {
        let temp = tempfile::tempdir().unwrap(); let p = Paths { root: temp.path().to_owned() };
        let key = include_bytes!("../packaging/release-key.pem");
        bootstrap_at(&p, key).unwrap(); bootstrap_at(&p, key).unwrap();
        assert_eq!(source(&p).unwrap().repository, "helloworlde/cowplay");
        assert!(bootstrap_at(&p, b"different key").is_err());
        assert_eq!(read(&p.at("etc/cowplay/release-key.pem"), 16384).unwrap(), key);
        write(&p.at("etc/cowplay/releases.json"), b"{\"repository\":\"example/other\"}").unwrap();
        assert!(bootstrap_at(&p, key).is_err());
        assert_eq!(source(&p).unwrap().repository, "example/other");
    }
    #[test]
    fn versions_sources_and_release_contract() {
        assert!(repository("example/cowplay"));
        for bad in [
            "../repo",
            "owner/..",
            "owner/repo/more",
            "https://github.com/a/b",
            "a/b?token=x",
            "a/b\n",
        ] {
            assert!(!repository(bad), "{bad}");
        }
        for bad in [
            "1.0-r0",
            "1.0.0",
            "1.0.0-r0/evil",
            "01.0.0-r0",
            "1.0.0-r1;id",
            "1.0.0-rc1",
        ] {
            assert!(version(bad).is_err());
        }
        let j = job();
        let m = manifest();
        let valid = |m: &Manifest, current: &BTreeMap<String, String>| {
            validate(m, &j.source, &j.tag, "aarch64_generic", "25.12.6", current)
        };
        assert!(valid(&m, &j.installed).is_ok());
        assert!(valid(&m, &BTreeMap::new()).is_ok());
        let mut mixed = j.installed.clone();
        mixed.insert("luci-app-cowplay".into(), "0.3.16-r0".into());
        assert!(valid(&m, &mixed).unwrap_err().contains("降级"));
        let mut mismatch = m.clone();
        mismatch.upgrade_from.clear();
        assert!(valid(&mismatch, &j.installed).is_err());
        for field in 0..8 {
            let mut bad = m.clone();
            match field {
                0 => bad.arch = "x86_64".into(),
                1 => bad.openwrt_releases = vec!["24.10".into()],
                2 => bad.repository = "attacker/repo".into(),
                3 => bad.packages[1].arch = "aarch64_generic".into(),
                4 => bad.packages[0].file = "../../evil.apk".into(),
                5 => bad.packages[0].size = MAX_PACKAGE + 1,
                6 => bad.packages[0].sha256 = "f".repeat(63),
                _ => bad.packages.swap(0, 1),
            };
            assert!(valid(&bad, &j.installed).is_err());
        }
    }
    #[test]
    fn release_policy_allows_reviewed_sources_and_rejects_unknown_or_newer_versions() {
        let policy: Value = serde_json::from_str(include_str!("../packaging/release-policy.json")).unwrap();
        let mut m = manifest();
        m.repository = policy["repository"].as_str().unwrap().into();
        m.version = include_str!("../packaging/VERSION").trim().into();
        m.tag = release_tag(&m.version);
        m.upgrade_from = serde_json::from_value(policy["upgrade_from"].clone()).unwrap();
        assert!(!m.upgrade_from.is_empty(), "remote updates need reviewed source versions");
        for p in &mut m.packages { p.file = format!("{}-{}-{}.apk", p.name, m.version, p.arch); }
        let source = Source { repository: m.repository.clone() };
        let valid = |manifest: &Manifest, old: &str| validate(manifest, &source, &m.tag, "aarch64_generic", "25.12.6", &NAMES.iter().map(|name| ((*name).into(), old.into())).collect());
        for old in &m.upgrade_from {
            valid(&m, old).unwrap();
            let mut missing = m.clone(); missing.upgrade_from.clear();
            assert!(valid(&missing, old).is_err());
        }
        assert!(valid(&m, "0.0.0-r0").is_err());
        assert!(valid(&m, "99.0.0-r0").unwrap_err().contains("降级"));
    }
    #[test]
    fn development_and_stable_releases_never_cross_channels() {
        let stable = json!({"tag_name":"v0.0.2","draft":false,"prerelease":false});
        let dev = json!({"tag_name":"v0.0.3","draft":false,"prerelease":true});
        assert!(select_release(&stable, "", Channel::Stable).is_ok());
        assert!(select_release(&stable, "v0.0.2", Channel::Stable).is_ok());
        assert!(select_release(&dev, "v0.0.3", Channel::Stable).is_err());
        assert!(select_release(&stable, "v0.0.2", Channel::Dev).is_err());
        assert!(select_release(&dev, "v0.0.3", Channel::Dev).is_ok());
        assert!(select_release(&dev, "v0.0.4", Channel::Dev).is_err());
        for invalid in [json!({"tag_name":"v0.0.3","draft":true,"prerelease":true}),
            json!({"tag_name":"v0.0.3","draft":false}),
            json!({"tag_name":"v0.0.3","draft":false,"prerelease":"true"})] {
            assert!(select_release(&invalid, "v0.0.3", Channel::Dev).is_err());
        }
        let list = json!([stable, dev,
            {"tag_name":"v0.0.10","draft":false,"prerelease":true},
            {"tag_name":"v9.0.0","draft":true,"prerelease":true},
            {"tag_name":"dev","draft":false,"prerelease":true}]);
        assert_eq!(select_release(&list, "", Channel::Dev).unwrap()["tag_name"], "v0.0.10");
        assert!(select_release(&json!([]), "", Channel::Dev).is_err());
        assert!(serde_json::from_value::<Channel>(json!("unknown")).is_err());
        let mut legacy = serde_json::to_value(job()).unwrap();
        legacy.as_object_mut().unwrap().remove("channel");
        assert!(serde_json::from_value::<Job>(legacy).unwrap().channel == Channel::Stable);
    }
    #[test]
    fn github_redirect_allowlist_and_path_input() {
        for url in [
            "https://github.com/a/b",
            "https://api.github.com/repos/a/b/releases",
            "https://release-assets.githubusercontent.com/x?signature=test",
            "https://objects.githubusercontent.com/x",
        ] {
            assert!(host_allowed(&url.parse().unwrap()));
        }
        for url in [
            "http://github.com/a",
            "https://github.com.attacker.test/x",
            "https://github.com:8443/x",
            "https://user:pass@github.com/x",
            "https://127.0.0.1/x",
            "file:///etc/passwd",
        ] {
            assert!(!host_allowed(&url.parse().unwrap()));
        }
        assert!(Paths::live().job("../private").is_err());
    }
    #[tokio::test]
    async fn integrity_signature_metadata_and_dependencies_fail_before_stop() {
        for mode in 0..4 {
            let (_tmp, p, mut j) = fixture();
            let m = j.manifest.clone().unwrap();
            match mode {
                0 => fs::write(p.job(&j.id).unwrap().join(&m.packages[0].file), b"evil").unwrap(),
                1 => fs::write(p.at("bad_signature"), b"").unwrap(),
                2 => fs::write(
                    p.at(&format!("{}.meta", m.packages[0].file)),
                    b"{\"info\":{\"name\":\"other\"}}",
                )
                .unwrap(),
                _ => fs::write(p.at("bad_dependencies"), b"").unwrap(),
            };
            assert!(apply_verified(&p, &mut j, &m).await.is_err(), "mode {mode}");
            assert!(!events(&p).contains("stop"));
            assert!(!events(&p).contains("install"));
            assert!(!j.applying);
        }
    }
    #[tokio::test]
    async fn preserves_private_state_and_previous_running_and_boot_preferences() {
        for running in [false, true] {
            for enabled in [false, true] {
                let (_tmp, p, mut j) = fixture();
                if running {
                    fs::write(p.at("running"), b"").unwrap();
                }
                if enabled {
                    fs::write(p.at("enabled"), b"").unwrap();
                }
                let m = j.manifest.clone().unwrap();
                apply_verified(&p, &mut j, &m).await.unwrap();
                assert_eq!(j.phase, "complete");
                assert_eq!(p.at("running").exists(), running);
                assert_eq!(p.at("enabled").exists(), enabled);
                assert_eq!(
                    fs::read(p.at("etc/cowplay/state/identity")).unwrap(),
                    b"SYNTHETIC PRIVATE STATE"
                );
                let backup = Path::new(j.backup.as_ref().unwrap());
                assert!(backup.exists());
                assert_eq!(
                    fs::metadata(backup).unwrap().permissions().mode() & 0o777,
                    0o600
                );
                let e = events(&p);
                assert!(e.find("simulate").unwrap() < e.find("install").unwrap());
                if running {
                    assert!(e.find("stop").unwrap() < e.find("install").unwrap());
                    assert!(e.find("install").unwrap() < e.find("start").unwrap());
                } else {
                    assert!(!e.contains("start"));
                }
            }
        }
    }
    #[tokio::test]
    async fn first_install_enables_next_boot_without_starting() {
        let (_tmp, p, mut j) = fixture();
        j.installed.clear();
        fs::write(p.at("installed.json"), b"[]").unwrap();
        fs::remove_file(p.at("etc/init.d/cowplay")).unwrap();
        fs::write(p.at("first_install"), b"").unwrap();
        let m = j.manifest.clone().unwrap();
        apply_verified(&p, &mut j, &m).await.unwrap();
        assert!(p.at("enabled").exists());
        assert!(!p.at("running").exists());
        assert!(!events(&p).contains("start"));
    }
    #[tokio::test]
    async fn backup_stop_and_install_failure_are_observable_without_blind_rollback() {
        for mode in 0..3 {
            let (_tmp, p, mut j) = fixture();
            fs::write(p.at("running"), b"").unwrap();
            match mode {
                0 => fs::write(p.at("fail_stop"), b"").unwrap(),
                1 => {
                    fs::remove_file(p.at("bin/tar")).unwrap();
                    script(&p, "bin/tar", "exit 1");
                }
                _ => fs::write(p.at("fail_install"), b"").unwrap(),
            };
            let m = j.manifest.clone().unwrap();
            assert!(apply_verified(&p, &mut j, &m).await.is_err());
            assert_eq!(events(&p).contains("install"), mode == 2);
            assert_eq!(j.applying, mode == 2);
            assert!(!events(&p).contains("start"));
            assert_eq!(
                fs::read(p.at("etc/cowplay/state/identity")).unwrap(),
                b"SYNTHETIC PRIVATE STATE"
            );
        }
    }
    #[tokio::test]
    async fn process_and_hci_shutdown_timeout_prevents_install() {
        let (_tmp, p, mut j) = fixture();
        fs::create_dir_all(p.at("sys/class/bluetooth/hci0")).unwrap();
        let m = j.manifest.clone().unwrap();
        assert!(
            apply_verified(&p, &mut j, &m)
                .await
                .unwrap_err()
                .contains("HCI")
        );
        assert!(!events(&p).contains("install"));
    }
    #[test]
    fn persistent_interruption_requires_acknowledgement_and_keeps_backup() {
        let (_tmp, p, mut j) = fixture();
        j.phase = "installing".into();
        j.applying = true;
        j.queued = now() - 11;
        j.backup = Some("private-local-backup.tgz".into());
        save(&p, &j).unwrap();
        write(&p.jobs().join("latest"), j.id.as_bytes()).unwrap();
        write(&p.jobs().join("active"), j.id.as_bytes()).unwrap();
        let v = status(&p).unwrap();
        assert_eq!(v["phase"], "interrupted");
        assert_eq!(v["locked"], true);
        assert_eq!(v["recovery_required"], true);
        assert_eq!(v["backup"], "private-local-backup.tgz");
        let _lock = lock(&p.jobs().join("lock")).unwrap();
        assert!(lock(&p.jobs().join("lock")).is_err());
        assert!(p.jobs().join("active").exists());
    }
    #[tokio::test]
    async fn detached_manifest_signature_and_trust_fail_closed() {
        let (_tmp, p, j) = fixture();
        let dir = p.job(&j.id).unwrap();
        let openssl = if Path::new("/opt/homebrew/bin/openssl").exists() {
            "/opt/homebrew/bin/openssl"
        } else {
            "/usr/bin/openssl"
        };
        std::os::unix::fs::symlink(openssl, p.at("usr/bin/openssl")).unwrap();
        let key = dir.join("test-private.key");
        let cmd = |args: &[&str]| {
            assert!(
                std::process::Command::new(openssl)
                    .args(args)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success()
            );
        };
        cmd(&[
            "genpkey",
            "-algorithm",
            "RSA",
            "-pkeyopt",
            "rsa_keygen_bits:2048",
            "-out",
            key.to_str().unwrap(),
        ]);
        cmd(&[
            "pkey",
            "-in",
            key.to_str().unwrap(),
            "-pubout",
            "-out",
            p.at("etc/cowplay/release-key.pem").to_str().unwrap(),
        ]);
        let manifest = dir.join("cowplay-release.json");
        let sig = dir.join("cowplay-release.sig");
        fs::write(&manifest, serde_json::to_vec(&j.manifest.unwrap()).unwrap()).unwrap();
        cmd(&[
            "dgst",
            "-sha256",
            "-sign",
            key.to_str().unwrap(),
            "-out",
            sig.to_str().unwrap(),
            manifest.to_str().unwrap(),
        ]);
        assert!(verify_manifest(&p, &dir).await.is_ok());
        fs::write(&manifest, b"{}").unwrap();
        assert!(verify_manifest(&p, &dir).await.is_err());
        assert!(events(&p).is_empty());
        fs::set_permissions(
            p.at("etc/cowplay/releases.json"),
            fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        assert!(source(&p).is_err());
    }

    #[tokio::test]
    async fn worker_restores_old_service_only_before_package_application() {
        for installing in [false, true] {
            let (_tmp, p, mut j) = fixture();
            fs::write(p.at("running"), b"").unwrap();
            if installing {
                fs::write(p.at("fail_install"), b"").unwrap();
            } else {
                fs::remove_file(p.at("bin/tar")).unwrap();
                script(&p, "bin/tar", "exit 1");
            }
            write(&p.jobs().join("active"), j.id.as_bytes()).unwrap();
            let m = j.manifest.clone().unwrap();
            let result = apply_verified(&p, &mut j, &m).await;
            assert!(result.is_err());
            finish(&p, &mut j, result, false).await.unwrap();
            assert_eq!(j.phase, "failed");
            assert_eq!(p.at("running").exists(), !installing);
            assert_eq!(events(&p).contains("start"), !installing);
            assert!(p.jobs().join("active").exists());
            cli_at(&p, "acknowledge", &json!({"id":j.id}))
                .await
                .unwrap();
            assert!(!p.jobs().join("active").exists());
            assert_eq!(
                fs::read(p.at("etc/cowplay/state/identity")).unwrap(),
                b"SYNTHETIC PRIVATE STATE"
            );
        }
    }
    #[tokio::test]
    async fn installed_version_change_during_preflight_prevents_shutdown() {
        let (_tmp, p, mut j) = fixture();
        script(
            &p,
            "usr/bin/apk",
            &format!(
                "{}\n",
                r#"
for arg in "$@"; do case "$arg" in
 query) cat "$R/target.json"; exit;;
 verify) exit 0;;
 adbdump) for file in "$@"; do case "$file" in *.apk) cat "$R/$(basename "$file").meta"; exit;; esac; done;;
 add) echo simulate >> "$R/events"; exit 0;;
esac; done
"#
            ),
        );
        let m = j.manifest.clone().unwrap();
        assert!(
            apply_verified(&p, &mut j, &m)
                .await
                .unwrap_err()
                .contains("版本已变化")
        );
        assert!(!events(&p).contains("stop"));
    }
    #[tokio::test]
    async fn bounded_https_download_rejects_oversize_truncation_status_and_bad_ca() {
        use std::io::{Read, Write};
        for mode in 0..5 {
            let cert = rcgen::generate_simple_self_signed(vec!["github.com".into()]).unwrap();
            let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.cert.der().clone()],
                rustls_pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
            )
            .unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let session = rustls::ServerConnection::new(Arc::new(tls)).unwrap();
                let mut stream = rustls::StreamOwned::new(session, socket);
                let mut buffer = [0; 4096];
                if stream.read(&mut buffer).is_ok() {
                    let body = match mode {
                        0 => {
                            "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntest"
                        }
                        1 => {
                            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n8\r\ntoolarge\r\n0\r\n\r\n"
                        }
                        2 => "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nx",
                        _ => {
                            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        }
                    };
                    let _ = stream.write_all(body.as_bytes());
                    let _ = stream.flush();
                }
            });
            let mut builder = reqwest::Client::builder()
                .no_proxy()
                .https_only(true)
                .timeout(Duration::from_secs(3))
                .resolve("github.com", addr);
            if mode != 4 {
                builder = builder.add_root_certificate(
                    reqwest::Certificate::from_pem(cert.cert.pem().as_bytes()).unwrap(),
                );
            }
            let client = builder.build().unwrap();
            let dir = tempfile::tempdir().unwrap();
            let dest = dir.path().join("asset");
            let result = download(
                &client,
                "https://github.com/example/cowplay/releases/download/v0.3.15/asset",
                &dest,
                4,
            )
            .await;
            if mode == 0 {
                result.unwrap();
                assert_eq!(fs::read(dest).unwrap(), b"test");
            } else {
                assert!(result.is_err(), "mode {mode}");
            }
            server.join().unwrap();
        }
    }

    #[test]
    fn completed_jobs_release_stale_markers_but_failed_installs_keep_them() {
        for phase in ["ready", "current", "complete", "failed"] {
            let (_tmp, p, mut j) = fixture();
            j.phase = phase.into();
            j.applying = phase == "complete";
            save(&p, &j).unwrap();
            write(&p.jobs().join("latest"), j.id.as_bytes()).unwrap();
            write(&p.jobs().join("active"), j.id.as_bytes()).unwrap();
            assert_eq!(status(&p).unwrap()["locked"], false);
        }
        let (_tmp, p, mut j) = fixture();
        j.phase = "failed".into();
        j.applying = true;
        save(&p, &j).unwrap();
        write(&p.jobs().join("latest"), j.id.as_bytes()).unwrap();
        write(&p.jobs().join("active"), j.id.as_bytes()).unwrap();
        assert_eq!(status(&p).unwrap()["locked"], true);
    }
}
