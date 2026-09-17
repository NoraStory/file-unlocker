//! 自动更新：从 GitHub / Gitee 双源拉取版本清单，下载、校验 SHA256、
//! 启动安装程序。清单格式见仓库根目录 update.json。
//!
//! 安全说明：安装器通过 ShellExecuteW 以参数列表形式启动（不经 shell，
//! 无命令注入面），且路径仅接受 download_asset 下载并通过 SHA256
//! 校验的受信文件。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 更新清单（update.json）中的资产条目
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UpdateAsset {
    pub name: String,
    /// "installer" 优先；其余类型仅展示
    pub kind: String,
    pub url: String,
    pub sha256: String,
}

/// 更新清单
#[derive(Debug, Clone, Deserialize)]
struct UpdateManifest {
    pub version: String,
    pub notes: String,
    pub assets: Vec<UpdateAsset>,
}

/// 返回给前端的更新信息
#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub notes: String,
    pub assets: Vec<UpdateAsset>,
    /// 命中哪个源（github/gitee），用于日志与展示
    pub source: String,
}

/// 双更新源：GitHub 优先，Gitee 兜底（中国大陆网络环境）
/// Gitee 地址为占位镜像；如未来在 Gitee 建仓，更新此常量即可。
const SOURCES: &[(&str, &str)] = &[
    (
        "github",
        "https://raw.githubusercontent.com/NoraStory/file-unlocker/main/update.json",
    ),
    (
        "gitee",
        "https://gitee.com/NoraStory/file-unlocker/raw/main/update.json",
    ),
];

/// 解析 x.y.z 版本号为可比较的整数序列
fn parse_version(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect()
}

/// candidate 是否比 current 新（忽略 current 为空的开发场景）。
/// 段数不一致时短的一侧补 0："1.0" 与 "1.0.0" 相等，"0.3.1" 新于 "0.3"。
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let mut c = parse_version(candidate);
    let mut v = parse_version(current);
    if v.is_empty() {
        return !c.is_empty();
    }
    let n = c.len().max(v.len());
    c.resize(n, 0);
    v.resize(n, 0);
    c > v
}

/// 依次尝试各更新源拉取清单，返回比当前版本新的更新信息。
/// 契约：网络/解析失败返回 Err（附最后一个源的原因），不再静默吞成"无更新"；
/// 源可达且清单有效时以它为准（不新则 Ok(None)，不再试下一个源）。
pub fn check_for_update(current: &str) -> Result<Option<UpdateInfo>, String> {
    let mut last_err = String::new();
    for (source, url) in SOURCES {
        log::info!("[更新] 检查源 {source}: {url}");
        let manifest = match fetch_manifest(url) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("[更新] 源 {source} 拉取失败: {e}");
                last_err = format!("{source}: {e}");
                continue;
            }
        };
        if is_newer(&manifest.version, current) {
            log::info!(
                "[更新] 源 {source} 发现新版本 {}（当前 {current}）",
                manifest.version
            );
            return Ok(Some(UpdateInfo {
                version: manifest.version,
                notes: manifest.notes,
                assets: manifest.assets,
                source: source.to_string(),
            }));
        }
        log::info!(
            "[更新] 源 {source} 版本 {} 不新于当前 {current}",
            manifest.version
        );
        return Ok(None); // 源可达且清单有效：以它为准，不再试下一个
    }
    Err(if last_err.is_empty() {
        "未配置更新源".to_string()
    } else {
        format!("全部更新源不可达（最后错误：{last_err}）")
    })
}

/// 拉取并解析更新清单；网络错误与清单格式错误都返回 Err（原因）
fn fetch_manifest(url: &str) -> Result<UpdateManifest, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(20))
        .build();
    let resp = agent
        .get(url)
        .call()
        .map_err(|e| format!("请求失败：{e}"))?;
    let text = resp
        .into_string()
        .map_err(|e| format!("读取响应失败：{e}"))?;
    match serde_json::from_str::<UpdateManifest>(&text) {
        Ok(m) if !m.assets.is_empty() => Ok(m),
        Ok(_) => Err("清单无资产条目".into()),
        Err(e) => Err(format!("清单解析失败：{e}")),
    }
}

/// 校验资产文件名：必须是纯文件名（防路径穿越），且不是 Windows 保留设备名
/// （NUL/CON/PRN/AUX/COM1-9/LPT1-9，不区分大小写、不含扩展名部分——
/// CON.txt、com1.exe 这类名字实际会写到设备上而非磁盘）
fn validate_asset_name(raw: &str) -> Result<String, String> {
    let name = std::path::Path::new(raw)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if name.is_empty() || name != raw {
        return Err(format!("非法的资产文件名：{raw}"));
    }
    let stem = name.split('.').next().unwrap_or("").to_lowercase();
    let reserved = matches!(stem.as_str(), "nul" | "con" | "prn" | "aux")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0');
    if reserved {
        return Err(format!("非法的资产文件名（Windows 保留名）：{raw}"));
    }
    Ok(name.to_string())
}

/// 下载安装包到临时目录，校验 SHA256，返回本地路径。
/// `on_progress(done, total)` 供进度条使用（total 未知时为 0）。
pub fn download_asset(
    asset: &UpdateAsset,
    on_progress: &(dyn Fn(u64, u64) + Sync),
) -> Result<std::path::PathBuf, String> {
    let dir = std::env::temp_dir().join("FileUnlockerUpdate");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建下载目录失败：{e}"))?;
    // 资产名必须是纯文件名且非保留设备名：清单内容来自网络，带路径分隔符、
    // ".." 或 CON/com1 之类的名字会把下载内容写到临时目录之外或落到设备上
    let name = validate_asset_name(&asset.name)?;
    let dest = dir.join(name);

    log::info!("[更新] 下载 {} ← {}", asset.name, asset.url);
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(120))
        .build();
    let resp = agent
        .get(&asset.url)
        .call()
        .map_err(|e| format!("下载请求失败：{e}"))?;

    let total: u64 = resp
        .header("Content-Length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(&dest).map_err(|e| format!("创建文件失败：{e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut done: u64 = 0;
    loop {
        use std::io::Read;
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("下载中断：{e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        use std::io::Write;
        file.write_all(&buf[..n])
            .map_err(|e| format!("写入失败：{e}"))?;
        done += n as u64;
        on_progress(done, total);
    }
    // 收尾无条件补一次最终进度：循环内只在读到数据时回调，
    // Content-Length 缺失（total=0）或末块不足一块时前端进度条会停住
    on_progress(done, total);

    let digest = format!("{:x}", hasher.finalize());
    if !digest.eq_ignore_ascii_case(&asset.sha256) {
        let _ = std::fs::remove_file(&dest);
        return Err(format!(
            "SHA256 校验失败：期望 {}，实际 {digest}",
            asset.sha256
        ));
    }
    log::info!(
        "[更新] 下载完成并通过校验: {} ({done} 字节)",
        dest.display()
    );
    Ok(dest)
}

/// 启动安装程序并在短暂等待后退出本程序（安装器替换 exe 前必须释放占用）。
///
/// 受信链校验：path 必须是 download_asset 产物（文件名以 FileUnlocker
/// 开头的 *-setup.exe 安装器）；启动使用 ShellExecuteW 参数列表调用，不经任何 shell。
pub fn launch_installer(path: &std::path::Path) -> Result<(), String> {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_lowercase();
    if !file_name.starts_with("fileunlocker") {
        return Err("拒绝启动：不是 FileUnlocker 安装程序".into());
    }
    // 只接受安装器：防止前端回退选中 portable 单文件 exe 时被当安装器启动
    if !file_name.ends_with("-setup.exe") {
        return Err("拒绝启动：不是 FileUnlocker 安装程序（应为 *-setup.exe）".into());
    }
    if !path.is_file() {
        return Err("安装程序文件不存在".into());
    }

    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let op = crate::winutil::to_wide("open");
    let file = crate::winutil::to_wide(&path.to_string_lossy());
    log::info!("[更新] 启动安装程序: {}", path.display());
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(op.as_ptr()),
            PCWSTR(file.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW 返回 HINSTANCE；<= 32 为错误码
    if (result.0 as isize) <= 32 {
        return Err(format!(
            "启动安装程序失败（ShellExecute 错误码 {}）",
            result.0 as isize
        ));
    }
    // 给安装器一点启动时间（NSIS 会自行弹出 UAC 确认），随后退出本进程
    std::thread::sleep(std::time::Duration::from_millis(1500));
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(is_newer("0.3.0", "0.2.1"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.2.1", "0.2.1"));
        assert!(!is_newer("0.2.0", "0.2.1"));
        assert!(!is_newer("0.2.1", "0.3.0"));
        // 段数不一致：短侧补 0 再比
        assert!(is_newer("0.3.1", "0.3"));
        assert!(!is_newer("1.0", "1.0.0"));
        assert!(!is_newer("0.3", "0.3.1"));
    }

    #[test]
    fn version_parse_tolerates_prefix() {
        assert!(is_newer("v0.3.0", "0.2.1"));
        assert!(is_newer("0.3.0", "v0.2.1"));
    }

    #[test]
    fn empty_current_accepts_any() {
        assert!(is_newer("0.1.0", ""));
        assert!(!is_newer("", "0.1.0"));
    }

    #[test]
    fn rejects_untrusted_launch_path() {
        // 非 FileUnlocker 前缀的路径必须被拒绝（防注入面）
        assert!(launch_installer(std::path::Path::new("C:\\evil.exe")).is_err());
        assert!(launch_installer(std::path::Path::new("C:\\temp\\hack.cmd")).is_err());
    }

    #[test]
    fn rejects_portable_exe_as_installer() {
        // FileUnlocker 前缀但非 -setup.exe（portable 单文件）同样拒绝
        assert!(launch_installer(std::path::Path::new("C:\\FileUnlocker_portable.exe")).is_err());
        // 合法安装器名但文件不存在：应进入存在性校验报错，而不是被启动
        assert!(launch_installer(std::path::Path::new("C:\\FileUnlocker_x64-setup.exe")).is_err());
    }

    #[test]
    fn asset_name_rejects_traversal_and_reserved() {
        assert!(validate_asset_name("FileUnlocker_x64-setup.exe").is_ok());
        // 路径穿越：带目录分隔符 / ".." / 空名
        assert!(validate_asset_name("..\\evil.exe").is_err());
        assert!(validate_asset_name("a/b.exe").is_err());
        assert!(validate_asset_name("").is_err());
        // Windows 保留设备名（不区分大小写、不含扩展名部分）
        for bad in [
            "CON", "con.txt", "NUL.exe", "PRN.dat", "AUX", "com1.exe", "COM9.bin", "lpt1",
            "LPT9.dll",
        ] {
            assert!(validate_asset_name(bad).is_err(), "应拒绝 {bad}");
        }
        // 非保留名不受影响（COM0/COM10 不在 COM1-9 保留区间）
        for good in [
            "com0.exe",
            "com10.exe",
            "lpt0.dat",
            "common.txt",
            "console.zip",
        ] {
            assert!(validate_asset_name(good).is_ok(), "应接受 {good}");
        }
    }
}
