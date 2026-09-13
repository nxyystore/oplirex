use std::path::{Path, PathBuf};
use tracing::{info, warn};

const RELEASES_LATEST_URL: &str = "https://api.github.com/repos/nxyystore/oplirex/releases/latest";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, serde::Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, serde::Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

fn asset_name_for_current_platform() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let os_str = match os {
        "linux" => "linux",
        "macos" => "macos",
        "windows" => "windows",
        other => other,
    };
    let arch_str = match arch {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "arm" => "arm64",
        other => other,
    };
    // primary candidate is tar.gz, fallback handled by caller
    format!("oplirex-{}-{}.tar.gz", os_str, arch_str)
}

fn normalize_version(v: &str) -> String {
    v.trim().trim_start_matches('v').trim().to_string()
}

fn is_newer(latest: &str, current: &str) -> bool {
    // simple semver compare: split by '.' and compare numeric parts
    let parse = |s: &str| {
        s.split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    let l = parse(&normalize_version(latest));
    let c = parse(&normalize_version(current));
    let len = l.len().max(c.len());
    for i in 0..len {
        let lv = *l.get(i).unwrap_or(&0);
        let cv = *c.get(i).unwrap_or(&0);
        if lv > cv {
            return true;
        } else if lv < cv {
            return false;
        }
    }
    false
}

/// Check only – fetch latest release and compare with current version.
/// Returns a human readable string.
pub async fn check_update() -> Result<String, String> {
    let latest = fetch_latest_release().await?;
    let latest_ver = normalize_version(&latest.tag_name);
    let current_ver = normalize_version(CURRENT_VERSION);
    if latest_ver == current_ver {
        Ok(format!("Already up to date (v{})", current_ver))
    } else if is_newer(&latest_ver, &current_ver) {
        Ok(format!(
            "Update available: v{} -> v{} ({} )",
            current_ver, latest_ver, latest.tag_name
        ))
    } else {
        // current newer than latest (e.g. dev build)
        Ok(format!(
            "Current v{} is newer than latest v{}",
            current_ver, latest_ver
        ))
    }
}

/// Self-update: fetch latest release, download asset for current OS/arch,
/// verify SHA256SUMS if present, extract to temp and replace current exe.
/// Handles Windows temp swap with .old.
/// Returns success message string.
pub async fn self_update(force: bool) -> Result<String, String> {
    let latest = fetch_latest_release().await?;
    let latest_ver = normalize_version(&latest.tag_name);
    let current_ver = normalize_version(CURRENT_VERSION);

    if !force && latest_ver == current_ver {
        return Ok(format!("Already up to date (v{})", current_ver));
    }
    if !force && !is_newer(&latest_ver, &current_ver) && latest_ver != current_ver {
        // if current is newer, don't downgrade unless forced
        return Ok(format!(
            "Current v{} is newer than latest v{} (use --force to downgrade)",
            current_ver, latest_ver
        ));
    }

    let asset_name = asset_name_for_current_platform();
    info!("Looking for asset {} in latest release {}", asset_name, latest.tag_name);

    // Try exact match, then try .zip fallback for windows, then try any oplirex- prefix
    let asset = find_asset(&latest.assets, &asset_name)
        .or_else(|| {
            // windows fallback: .zip
            if asset_name.ends_with(".tar.gz") {
                let zip_name = asset_name.replace(".tar.gz", ".zip");
                find_asset(&latest.assets, &zip_name)
            } else {
                None
            }
        })
        .or_else(|| {
            // fallback: first asset that starts with oplirex-
            latest
                .assets
                .iter()
                .find(|a| a.name.starts_with("oplirex-"))
        });

    let asset = match asset {
        Some(a) => a,
        None => {
            let available: Vec<String> = latest.assets.iter().map(|a| a.name.clone()).collect();
            return Err(format!(
                "No asset found for current platform (looked for {}). Available: {}",
                asset_name,
                available.join(", ")
            ));
        }
    };

    info!("Downloading asset {} from {}", asset.name, asset.browser_download_url);

    let client = reqwest::Client::builder()
        .user_agent(format!("oplirex/{}", CURRENT_VERSION))
        .build()
        .map_err(|e| e.to_string())?;

    let bytes = download_bytes(&client, &asset.browser_download_url).await?;

    // Verify SHA256SUMS if available
    if let Some(sums_asset) = latest
        .assets
        .iter()
        .find(|a| a.name.to_lowercase().contains("sha256sums") || a.name.to_lowercase().contains("checksums") || a.name == "SHA256SUMS")
    {
        info!("Found checksum file {}, verifying...", sums_asset.name);
        match download_bytes(&client, &sums_asset.browser_download_url).await {
            Ok(sums_bytes) => {
                let sums_text = String::from_utf8_lossy(&sums_bytes);
                if let Err(e) = verify_sha256(&asset.name, &bytes, &sums_text) {
                    return Err(format!("SHA256 verification failed: {}", e));
                }
                info!("SHA256 verified for {}", asset.name);
            }
            Err(e) => {
                warn!("Failed to download SHA256SUMS ({}), skipping verification: {}", sums_asset.name, e);
            }
        }
    } else {
        warn!("No SHA256SUMS asset found, skipping verification");
    }

    // Extract to temp
    let temp_dir = std::env::temp_dir().join(format!("oplirex-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;

    let new_binary_path = if asset.name.ends_with(".zip") {
        // zip not expected but handle via fallback: write and error
        return Err("zip asset extraction not supported in this build (expected tar.gz)".to_string());
    } else {
        extract_tar_gz(&bytes, &temp_dir)?
    };

    // Replace current exe
    let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    info!("Replacing current exe {:?} with {:?}", current_exe, new_binary_path);
    replace_executable(&new_binary_path, &current_exe)?;

    // cleanup temp
    let _ = std::fs::remove_dir_all(&temp_dir);

    Ok(format!(
        "Updated v{} -> v{} (asset {})",
        current_ver, latest_ver, asset.name
    ))
}

fn find_asset<'a>(assets: &'a [GithubAsset], name: &str) -> Option<&'a GithubAsset> {
    assets.iter().find(|a| a.name == name)
}

async fn fetch_latest_release() -> Result<GithubRelease, String> {
    let client = reqwest::Client::builder()
        .user_agent(format!("oplirex/{}", CURRENT_VERSION))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(RELEASES_LATEST_URL)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("GitHub API {}: {}", status, text));
    }
    let release = resp
        .json::<GithubRelease>()
        .await
        .map_err(|e| e.to_string())?;
    Ok(release)
}

async fn download_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("Download failed {}: {}", resp.status(), url));
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    Ok(bytes.to_vec())
}

fn verify_sha256(asset_name: &str, data: &[u8], sums_text: &str) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    let hash = hasher.finalize();
    let hex_hash = hex::encode(hash);

    // SHA256SUMS format: "<hash>  <filename>" per line
    for line in sums_text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let file = parts[1].trim_start_matches('*');
            // match exact or basename
            if file == asset_name || file.ends_with(asset_name) || asset_name.ends_with(file) {
                if parts[0].to_lowercase() == hex_hash.to_lowercase() {
                    return Ok(());
                } else {
                    return Err(format!(
                        "hash mismatch for {}: expected {}, got {}",
                        asset_name, parts[0], hex_hash
                    ));
                }
            }
        }
    }
    // If file not found in sums, treat as error if sums file is small (strict), else warn?
    // For safety, if asset not listed, return error.
    Err(format!(
        "asset {} not found in SHA256SUMS (computed {})",
        asset_name, hex_hash
    ))
}

fn extract_tar_gz(bytes: &[u8], dest: &Path) -> Result<PathBuf, String> {
    use flate2::read::GzDecoder;
    use tar::Archive;
    let gz = GzDecoder::new(bytes);
    let mut archive = Archive::new(gz);
    archive.unpack(dest).map_err(|e| format!("tar unpack failed: {}", e))?;

    // Find binary: look for oplirex or oplirex.exe
    let candidates = find_binary(dest)?;
    // Prefer exact file that is executable
    // If multiple, pick the one with deepest match or largest size
    let best = candidates
        .into_iter()
        .max_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .ok_or_else(|| "no binary found in archive".to_string())?;
    Ok(best)
}

fn find_binary(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = std::fs::read_dir(&d).map_err(|e| e.to_string())?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name == "oplirex" || name == "oplirex.exe" || name.starts_with("oplirex-") {
                        // Check if it's likely binary (executable bit or exe)
                        out.push(path);
                    } else if name == "oplirex" || name.contains("oplirex") {
                        out.push(path);
                    }
                }
            }
        }
    }
    // fallback: if no candidate with oplirex name, return any file
    if out.is_empty() {
        // try any file in dest
        let mut stack2 = vec![dir.to_path_buf()];
        while let Some(d) = stack2.pop() {
            if let Ok(entries) = std::fs::read_dir(&d) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        stack2.push(p);
                    } else if p.is_file() {
                        out.push(p);
                    }
                }
            }
        }
        if out.is_empty() {
            return Err("archive empty".to_string());
        }
        // filter to most likely: largest file
        out.sort_by_key(|p| std::cmp::Reverse(std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)));
        // return just the largest
        return Ok(vec![out[0].clone()]);
    }
    Ok(out)
}

fn replace_executable(new_bin: &Path, current_exe: &Path) -> Result<(), String> {
    // Ensure new binary is executable on unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(new_bin)
            .map_err(|e| e.to_string())?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(new_bin, perms).map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "windows")]
    {
        // Windows: running exe is locked. Use .old swap.
        let old_path = current_exe.with_extension("old");
        // Also try .exe.old for clarity
        let old_path2 = {
            let mut p = current_exe.as_os_str().to_os_string();
            p.push(".old");
            PathBuf::from(p)
        };
        let old = if old_path2.exists() || !old_path.exists() {
            // Prefer .old suffix append to avoid extension confusion
            old_path2
        } else {
            old_path
        };
        // Remove previous .old if exists
        let _ = std::fs::remove_file(&old);
        // Try rename current -> old (may fail if locked, fallback to copy)
        match std::fs::rename(current_exe, &old) {
            Ok(()) => {
                // Now copy new to current
                std::fs::copy(new_bin, current_exe).map_err(|e| {
                    // try to restore
                    let _ = std::fs::rename(&old, current_exe);
                    format!("failed to copy new binary to {:?}: {}", current_exe, e)
                })?;
                info!("Windows update: moved current to {:?}, replaced", old);
                Ok(())
            }
            Err(e) => {
                warn!("rename to .old failed ({}), trying direct copy with temp swap", e);
                // Fallback: copy new to temp, then attempt to replace via std::fs::copy (may fail if locked)
                // Try to copy new_bin over current_exe directly
                // If that fails due to lock, write to adjacent file and instruct restart
                match std::fs::copy(new_bin, current_exe) {
                    Ok(_) => Ok(()),
                    Err(copy_err) => {
                        // Last resort: write to temp next to exe
                        let temp_new = current_exe.with_extension("new");
                        std::fs::copy(new_bin, &temp_new).map_err(|e2| {
                            format!(
                                "windows replace failed (rename: {}, copy: {}, temp copy: {})",
                                e, copy_err, e2
                            )
                        })?;
                        Err(format!(
                            "Updated binary staged at {:?} (current exe locked). Restart to complete. Copy error was: {}",
                            temp_new, copy_err
                        ))
                    }
                }
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Unix: atomic replace via copy + chmod
        // Try rename, fallback to copy
        // Use copy to preserve atomicity when overwrite
        std::fs::copy(new_bin, current_exe).map_err(|e| e.to_string())?;
        // Ensure executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(current_exe)
                .map_err(|e| e.to_string())?
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(current_exe, perms).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
