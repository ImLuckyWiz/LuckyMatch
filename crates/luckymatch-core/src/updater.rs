use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use crate::error::CoreError;

pub const GITHUB_REPO: &str = "ImLuckyWiz/LuckyMatch";
pub const WINDOWS_INSTALLER_ASSET: &str = "luckymatch-setup-x64.exe";
pub const WINDOWS_ZIP_ASSET: &str = "luckymatch-windows-x64.zip";
pub const ANDROID_APK_ASSET: &str = "luckymatch-android.apk";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub download_url: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub tag_name: String,
    pub version: String,
    pub name: String,
    pub notes: String,
    pub html_url: String,
    pub assets: Vec<ReleaseAsset>,
}

impl ReleaseInfo {
    /// Returns the preferred Windows installer `.exe` asset if present.
    pub fn windows_installer_asset(&self) -> Option<&ReleaseAsset> {
        self.assets
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(WINDOWS_INSTALLER_ASSET))
            .or_else(|| {
                self.assets
                    .iter()
                    .find(|a| a.name.to_ascii_lowercase().ends_with(".exe"))
            })
    }

    /// Returns the Android `.apk` asset if present.
    pub fn android_apk_asset(&self) -> Option<&ReleaseAsset> {
        self.assets
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(ANDROID_APK_ASSET))
            .or_else(|| {
                self.assets
                    .iter()
                    .find(|a| a.name.to_ascii_lowercase().ends_with(".apk"))
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCheckResult {
    UpToDate { current_version: String },
    UpdateAvailable(ReleaseInfo),
}

#[derive(Debug, Deserialize)]
struct GithubReleaseJson {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GithubAssetJson>,
}

#[derive(Debug, Deserialize)]
struct GithubAssetJson {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// Parses a `vMAJOR.MINOR.PATCH` or `MAJOR.MINOR` string into a numeric tuple.
pub fn parse_semver(version: &str) -> Option<(u64, u64, u64)> {
    let trimmed = version.trim();
    let stripped = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);
    let core = stripped
        .split(['-', '+'])
        .next()
        .unwrap_or("")
        .trim();
    if core.is_empty() {
        return None;
    }

    let mut parts = core.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next()?.parse().ok()?;
    let patch: u64 = match parts.next() {
        Some(p) => p.parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }

    Some((major, minor, patch))
}

/// Returns `true` when `latest` parses to a strictly higher semver than `current`.
pub fn is_newer_version(latest: &str, current: &str) -> bool {
    match (parse_semver(latest), parse_semver(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

fn parse_release_payload(body: &str) -> Result<Option<ReleaseInfo>, CoreError> {
    let raw: GithubReleaseJson =
        serde_json::from_str(body).map_err(|e| CoreError::Json(format!("release JSON: {e}")))?;
    if raw.draft || raw.prerelease {
        return Ok(None);
    }

    let version = raw
        .tag_name
        .trim()
        .strip_prefix('v')
        .or_else(|| raw.tag_name.trim().strip_prefix('V'))
        .unwrap_or(raw.tag_name.trim())
        .to_string();

    let name = raw
        .name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| raw.tag_name.clone());

    let assets = raw
        .assets
        .into_iter()
        .map(|a| ReleaseAsset {
            name: a.name,
            download_url: a.browser_download_url,
            size: a.size,
        })
        .collect();

    Ok(Some(ReleaseInfo {
        tag_name: raw.tag_name,
        version,
        name,
        notes: raw.body.unwrap_or_default(),
        html_url: raw.html_url,
        assets,
    }))
}

/// Checks GitHub Releases (`releases/latest`) for an update newer than `current_version`.
pub async fn check_latest_release(
    client: &reqwest::Client,
    current_version: &str,
) -> Result<UpdateCheckResult, CoreError> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", format!("LuckyMatch/{current_version}"))
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| CoreError::Http(format!("update check failed: {e}")))?;

    if !resp.status().is_success() {
        return Err(CoreError::Http(format!(
            "GitHub release check returned HTTP {}",
            resp.status()
        )));
    }

    let body = resp
        .text()
        .await
        .map_err(|e| CoreError::Http(format!("reading release response failed: {e}")))?;

    let Some(info) = parse_release_payload(&body)? else {
        return Ok(UpdateCheckResult::UpToDate {
            current_version: current_version.to_string(),
        });
    };

    if is_newer_version(&info.version, current_version) {
        Ok(UpdateCheckResult::UpdateAvailable(info))
    } else {
        Ok(UpdateCheckResult::UpToDate {
            current_version: current_version.to_string(),
        })
    }
}

/// Downloads a release asset to `dest_path` via a `.tmp` sibling file, reporting `(downloaded, total)`.
pub async fn download_asset_to_file<F>(
    client: &reqwest::Client,
    url: &str,
    dest_path: &Path,
    expected_size: Option<u64>,
    mut on_progress: F,
) -> Result<(), CoreError>
where
    F: FnMut(u64, Option<u64>),
{
    if let Some(parent) = dest_path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }

    let resp = client
        .get(url)
        .header("User-Agent", "LuckyMatch-Updater")
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|e| CoreError::Http(format!("downloading update failed: {e}")))?;

    if !resp.status().is_success() {
        return Err(CoreError::Http(format!(
            "download returned HTTP {}",
            resp.status()
        )));
    }

    let total = resp
        .content_length()
        .or_else(|| expected_size.filter(|&s| s > 0));

    let tmp_name = format!(
        "{}.tmp",
        dest_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("update.bin")
    );
    let tmp_path = dest_path.with_file_name(tmp_name);

    let download_res: Result<u64, CoreError> = async {
        let mut file = tokio::fs::File::create(&tmp_path).await?;
        let mut downloaded: u64 = 0;
        let mut resp = resp;

        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| CoreError::Http(format!("reading update stream failed: {e}")))?
        {
            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;
            on_progress(downloaded, total);
        }

        file.flush().await?;
        Ok(downloaded)
    }
    .await;

    let downloaded = match download_res {
        Ok(n) => n,
        Err(err) => {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(err);
        }
    };

    if downloaded == 0 {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(CoreError::Http("downloaded update file is empty".to_string()));
    }

    if let Some(expected) = expected_size.filter(|&s| s > 0) {
        if downloaded != expected {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(CoreError::Http(format!(
                "downloaded size mismatch: got {downloaded} bytes, expected {expected}"
            )));
        }
    }

    if tokio::fs::try_exists(dest_path).await.unwrap_or(false) {
        let _ = tokio::fs::remove_file(dest_path).await;
    }
    tokio::fs::rename(&tmp_path, dest_path).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_semver_variants() {
        assert_eq!(parse_semver("0.1.2"), Some((0, 1, 2)));
        assert_eq!(parse_semver("v0.1.3"), Some((0, 1, 3)));
        assert_eq!(parse_semver("V1.2.3-beta.1+build7"), Some((1, 2, 3)));
        assert_eq!(parse_semver("2.4"), Some((2, 4, 0)));
        assert_eq!(parse_semver("invalid"), None);
        assert_eq!(parse_semver(""), None);
        assert_eq!(parse_semver("1.2.3.4"), None);
    }

    #[test]
    fn compares_versions_correctly() {
        assert!(is_newer_version("v0.1.3", "0.1.2"));
        assert!(is_newer_version("0.2.0", "0.1.9"));
        assert!(is_newer_version("1.0.0", "0.9.9"));
        assert!(!is_newer_version("v0.1.2", "0.1.2"));
        assert!(!is_newer_version("0.1.1", "0.1.2"));
        assert!(!is_newer_version("not-a-ver", "0.1.2"));
    }

    #[test]
    fn parses_github_release_and_selects_platform_assets() {
        let payload = r#"{
            "tag_name": "v0.1.3",
            "name": "LuckyMatch v0.1.3",
            "body": "Auto update support",
            "html_url": "https://github.com/ImLuckyWiz/LuckyMatch/releases/tag/v0.1.3",
            "draft": false,
            "prerelease": false,
            "assets": [
                {
                    "name": "luckymatch-android.apk",
                    "browser_download_url": "https://example.com/luckymatch-android.apk",
                    "size": 31000000
                },
                {
                    "name": "luckymatch-setup-x64.exe",
                    "browser_download_url": "https://example.com/luckymatch-setup-x64.exe",
                    "size": 9500000
                },
                {
                    "name": "luckymatch-windows-x64.zip",
                    "browser_download_url": "https://example.com/luckymatch-windows-x64.zip",
                    "size": 11500000
                }
            ]
        }"#;

        let info = parse_release_payload(payload)
            .expect("valid JSON")
            .expect("non-draft release");
        assert_eq!(info.tag_name, "v0.1.3");
        assert_eq!(info.version, "0.1.3");
        assert_eq!(
            info.windows_installer_asset().map(|a| a.name.as_str()),
            Some("luckymatch-setup-x64.exe")
        );
        assert_eq!(
            info.android_apk_asset().map(|a| a.name.as_str()),
            Some("luckymatch-android.apk")
        );
    }
}
