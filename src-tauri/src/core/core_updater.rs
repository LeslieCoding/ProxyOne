//! Periodic auto-update of the mihomo core binary (`verge-mihomo`).
//!
//! Storage layout under the app home dir (`%APPDATA%\com.proxyone.app`):
//!   cores/verge-mihomo.exe  — the managed override binary, preferred over the bundled sidecar
//!   cores/version.txt       — the mihomo release tag the override was installed from
//!
//! Version baseline: when an override binary exists, `version.txt` is authoritative. When no
//! override exists, the bundled sidecar is queried with `-v` to learn its version. If that query
//! fails the baseline is unknown and the latest release counts as newer — the staged binary is
//! verified with `-v` before it replaces anything, and the bundled sidecar always remains as
//! fallback, so this is safe.

use crate::utils::dirs;
use anyhow::Result;
use clash_verge_logging::{Type, logging};
use std::path::PathBuf;
use tauri::AppHandle;
use tauri_plugin_shell::{ShellExt as _, process::Command};

#[cfg(windows)]
const OVERRIDE_EXE_NAME: &str = "verge-mihomo.exe";
#[cfg(not(windows))]
const OVERRIDE_EXE_NAME: &str = "verge-mihomo";

/// Directory holding the managed (auto-updated) core binary.
pub fn cores_dir() -> Result<PathBuf> {
    Ok(dirs::app_home_dir()?.join("cores"))
}

/// Path of the auto-updated core override, if one is installed.
pub fn override_core_path() -> Option<PathBuf> {
    let path = cores_dir().ok()?.join(OVERRIDE_EXE_NAME);
    path.is_file().then_some(path)
}

/// Build the command used to run the given core.
///
/// For `verge-mihomo`, an auto-updated binary in the cores dir takes precedence over the
/// bundled sidecar; everything else (and any missing override) falls back to the sidecar.
pub fn core_command(app_handle: &AppHandle, clash_core: &str) -> Result<Command> {
    if clash_core == "verge-mihomo"
        && let Some(path) = override_core_path()
    {
        logging!(info, Type::Core, "Using auto-updated mihomo core: {}", path.display());
        return Ok(app_handle.shell().command(dirs::path_to_str(&path)?));
    }

    app_handle
        .shell()
        .sidecar(clash_core)
        .map_err(|error| anyhow::anyhow!("failed to build sidecar command for core {clash_core:?}: {error:#}"))
}

/// Start the periodic core auto-update scheduler (no-op on unsupported platforms).
pub fn init() {
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        logging!(info, Type::Core, "Starting mihomo core auto-update scheduler");
        imp::spawn_scheduler();
    }
    #[cfg(not(all(windows, target_arch = "x86_64")))]
    logging!(
        debug,
        Type::Core,
        "Mihomo core auto-update is not supported on this platform"
    );
}

#[cfg(all(windows, target_arch = "x86_64"))]
mod imp {
    use super::{cores_dir, override_core_path};
    use crate::{
        AsyncHandler,
        config::Config,
        core::{CoreManager, manager::RunningMode},
        utils::network::{NetworkManager, ProxyType},
    };
    use anyhow::{Context as _, Result};
    use clash_verge_logging::{Type, logging};
    use scopeguard::defer;
    use serde::Deserialize;
    use std::{
        io::{Cursor, Read as _},
        os::windows::process::CommandExt as _,
        path::{Path, PathBuf},
        process::Command as StdCommand,
        sync::atomic::{AtomicBool, Ordering},
        time::Duration,
    };

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const RELEASES_LATEST_API: &str = "https://api.github.com/repos/MetaCubeX/mihomo/releases/latest";
    const ASSET_BASE: &str = "mihomo-windows-amd64-v2";
    const OVERRIDE_EXE_NAME: &str = "verge-mihomo.exe";
    const STAGED_EXE_NAME: &str = "verge-mihomo.exe.new";
    const VERSION_FILE_NAME: &str = "version.txt";
    const STARTUP_DELAY: Duration = Duration::from_secs(60);
    const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

    static RUNNING: AtomicBool = AtomicBool::new(false);

    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
        assets: Vec<ReleaseAsset>,
    }

    #[derive(Deserialize)]
    struct ReleaseAsset {
        name: String,
        browser_download_url: String,
    }

    pub fn spawn_scheduler() {
        AsyncHandler::spawn(|| async {
            tokio::time::sleep(STARTUP_DELAY).await;
            loop {
                if let Err(error) = run_once().await {
                    logging!(warn, Type::Core, "Core auto-update check failed: {error:#}");
                }
                tokio::time::sleep(CHECK_INTERVAL).await;
            }
        });
    }

    async fn run_once() -> Result<()> {
        let verge = Config::verge().await;
        let verge = verge.latest_arc();
        let enabled = verge.enable_core_auto_update.unwrap_or(true);
        let clash_core = verge.get_valid_clash_core();
        drop(verge);

        if !enabled {
            logging!(debug, Type::Core, "Core auto-update is disabled, skipping");
            return Ok(());
        }
        if clash_core != "verge-mihomo" {
            logging!(
                debug,
                Type::Core,
                "Core auto-update only manages verge-mihomo, current: {clash_core}"
            );
            return Ok(());
        }
        if RUNNING.swap(true, Ordering::SeqCst) {
            logging!(debug, Type::Core, "Core auto-update already running, skipping");
            return Ok(());
        }
        defer! {
            RUNNING.store(false, Ordering::SeqCst);
        }

        let release = fetch_latest_release().await?;
        let baseline = current_baseline();
        if let Some(current) = baseline.as_deref()
            && !tag_newer(&release.tag_name, current)
        {
            logging!(
                debug,
                Type::Core,
                "Mihomo core is up to date ({current}), latest release is {}",
                release.tag_name
            );
            return Ok(());
        }

        logging!(
            info,
            Type::Core,
            "New mihomo core release {} available (current: {}), updating",
            release.tag_name,
            baseline.as_deref().unwrap_or("unknown")
        );
        stage_and_verify(&release).await?;
        commit_staged(&release.tag_name).await
    }

    async fn fetch_latest_release() -> Result<Release> {
        let resp = NetworkManager::new()
            .get_with_interrupt(RELEASES_LATEST_API, ProxyType::System, Some(30), None, false)
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("GitHub releases/latest returned {}", resp.status());
        }
        Ok(serde_json::from_str(resp.text_with_charset()?)?)
    }

    /// Tag of the core currently in use: the override's recorded tag, or the bundled sidecar's
    /// own version when no override has been installed yet.
    fn current_baseline() -> Option<String> {
        if override_core_path().is_some() {
            return cores_dir()
                .ok()
                .and_then(|cores| std::fs::read_to_string(cores.join(VERSION_FILE_NAME)).ok())
                .map(|tag| tag.trim().to_owned())
                .filter(|tag| !tag.is_empty());
        }
        bundled_core_version()
    }

    fn bundled_sidecar_path() -> Result<PathBuf> {
        let exe = tauri::utils::platform::current_exe()?;
        let dir = exe
            .parent()
            .ok_or_else(|| anyhow::anyhow!("failed to locate the executable directory"))?;
        let triple = tauri::utils::platform::target_triple()?;
        Ok(dir.join(format!("verge-mihomo-{triple}.exe")))
    }

    fn bundled_core_version() -> Option<String> {
        let path = bundled_sidecar_path().ok()?;
        if !path.is_file() {
            return None;
        }
        let output = StdCommand::new(&path)
            .arg("-v")
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        parse_version_token(&String::from_utf8_lossy(&output.stdout))
    }

    async fn stage_and_verify(release: &Release) -> Result<()> {
        let cores = cores_dir()?;
        let asset_name = format!("{ASSET_BASE}-{}.zip", release.tag_name);
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .or_else(|| {
                release
                    .assets
                    .iter()
                    .find(|asset| asset.name.starts_with(ASSET_BASE) && asset.name.ends_with(".zip"))
            })
            .ok_or_else(|| anyhow::anyhow!("release {} has no asset {asset_name}", release.tag_name))?;

        let client = NetworkManager::new()
            .create_request(ProxyType::System, Some(300), None, false)
            .await?;
        let bytes = client
            .get(&asset.browser_download_url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        logging!(
            info,
            Type::Core,
            "Downloaded core update {} ({} bytes)",
            asset.name,
            bytes.len()
        );

        let staged = cores.join(STAGED_EXE_NAME);
        let zip_bytes = bytes.to_vec();
        AsyncHandler::spawn_blocking(move || -> Result<()> {
            extract_exe(&zip_bytes, &staged)?;
            verify_exe(&staged)
        })
        .await
        .map_err(|error| anyhow::anyhow!("core staging task failed to complete: {error}"))??;
        Ok(())
    }

    fn extract_exe(zip_bytes: &[u8], staged: &Path) -> Result<()> {
        let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes))?;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            if entry.name().ends_with(".exe") {
                let mut buf = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut buf)?;
                if let Some(parent) = staged.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(staged, &buf)?;
                return Ok(());
            }
        }
        anyhow::bail!("no .exe entry found in the downloaded archive")
    }

    /// Run the staged binary with `-v`; on failure the staged file is removed so a later
    /// check retries from scratch.
    fn verify_exe(path: &Path) -> Result<()> {
        let output = StdCommand::new(path)
            .arg("-v")
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .context("failed to run the staged core binary")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() || parse_version_token(&stdout).is_none() {
            let _ = std::fs::remove_file(path);
            anyhow::bail!(
                "staged core failed the -v check (status {}): {}",
                output.status,
                stdout.trim()
            );
        }
        Ok(())
    }

    async fn commit_staged(tag: &str) -> Result<()> {
        let cores = cores_dir()?;
        let staged = cores.join(STAGED_EXE_NAME);
        let target = cores.join(OVERRIDE_EXE_NAME);

        // A running sidecar holds the exe file locked, and only sidecar mode uses the
        // override binary (service mode runs its own copy), so only then stop/start.
        let was_sidecar = matches!(*CoreManager::global().get_running_mode(), RunningMode::Sidecar);
        if was_sidecar {
            logging!(info, Type::Core, "Stopping core to apply mihomo update {tag}");
            CoreManager::global().stop_core().await?;
        }

        let replace_result = (|| -> Result<()> {
            std::fs::rename(&staged, &target).context("failed to replace the managed core binary")?;
            std::fs::write(cores.join(VERSION_FILE_NAME), tag).context("failed to record the core version")?;
            Ok(())
        })();

        if let Err(error) = replace_result {
            if was_sidecar {
                logging!(
                    warn,
                    Type::Core,
                    "Core update aborted ({error:#}), restarting the previous core"
                );
                if let Err(start_error) = CoreManager::global().start_core().await {
                    logging!(
                        error,
                        Type::Core,
                        "Failed to restart core after aborted update: {start_error:#}"
                    );
                }
            }
            return Err(error);
        }

        logging!(info, Type::Core, "Mihomo core updated to {tag}");
        if was_sidecar {
            CoreManager::global()
                .start_core()
                .await
                .context("core updated but failed to restart")?;
        }
        Ok(())
    }

    /// First `v<digits...>` token in a `-v` output such as
    /// "Mihomo Meta v1.19.31 windows amd64 with go1.25.x".
    fn parse_version_token(output: &str) -> Option<String> {
        output
            .split_whitespace()
            .find(|token| {
                token
                    .strip_prefix('v')
                    .is_some_and(|rest| rest.chars().next().is_some_and(|c| c.is_ascii_digit()))
            })
            .map(str::to_owned)
    }

    /// Semver-ish comparison of release tags like "v1.19.31".
    fn tag_newer(latest: &str, current: &str) -> bool {
        let parse = |tag: &str| -> Vec<u64> {
            tag.trim()
                .trim_start_matches('v')
                .split('.')
                .filter_map(|part| part.split('-').next().unwrap_or("0").parse().ok())
                .collect()
        };
        let (latest_parts, current_parts) = (parse(latest), parse(current));
        if latest_parts.is_empty() || current_parts.is_empty() {
            return latest != current;
        }
        for index in 0..latest_parts.len().max(current_parts.len()) {
            let a = latest_parts.get(index).copied().unwrap_or(0);
            let b = current_parts.get(index).copied().unwrap_or(0);
            if a != b {
                return a > b;
            }
        }
        false
    }

    #[cfg(test)]
    mod tests {
        use super::{parse_version_token, tag_newer};

        #[test]
        fn tag_newer_compares_numeric_components() {
            assert!(tag_newer("v1.19.32", "v1.19.31"));
            assert!(tag_newer("v1.20.0", "v1.19.31"));
            assert!(tag_newer("v2.0.0", "1.19.31"));
            assert!(!tag_newer("v1.19.31", "v1.19.31"));
            assert!(!tag_newer("v1.19.31", "v1.19.32"));
            assert!(!tag_newer("v1.19.3", "v1.19.31"));
        }

        #[test]
        fn parse_version_token_reads_mihomo_output() {
            let output = "Mihomo Meta v1.19.31 windows amd64 with go1.25.1 Sat Jan  1 00:00:00 UTC 2026\nUse flags: -v";
            assert_eq!(parse_version_token(output).as_deref(), Some("v1.19.31"));
            assert_eq!(parse_version_token("no version here"), None);
        }
    }
}
