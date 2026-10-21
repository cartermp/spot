use anyhow::{ anyhow, bail, Context, Result };
use serde::{ Deserialize, Serialize };
use sha2::{ Digest, Sha256 };
use spot_core::{
    git_shas_match_verified,
    verify_update_digest_verified,
    FormalInvariant,
    StoredUpdateEntry,
    UpdateAvailability,
    UpdateCatalogStore,
    UpdateReleaseManifest,
    MAX_STORED_UPDATES,
};
use std::path::{ Path, PathBuf };

pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BUILD_COMMIT_SHA: &str = env!("SPOT_BUILD_COMMIT");
pub const BUILD_SHORT_SHA: &str = env!("SPOT_BUILD_SHORT_COMMIT");
pub const BUILD_TIMESTAMP: &str = env!("SPOT_BUILD_TIMESTAMP");

pub fn compiled_build_number() -> u64 {
    env!("SPOT_BUILD_NUMBER").trim().parse::<u64>().unwrap_or(1).max(1)
}

pub fn default_github_repo() -> String {
    std::env
        ::var("SPOT_UPDATE_REPO")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "cartermp/spot".to_string())
}

pub fn spot_data_dir() -> PathBuf {
    let is_test = std::env::var("SPOT_TEST_CACHE_DIR").is_ok()
        || std::env::var("GROOVE_TEST_CACHE_DIR").is_ok()
        || cfg!(test);
    if is_test {
        return std::env::temp_dir().join("spot-test-cache");
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Spot")
}

pub fn updates_root_dir() -> PathBuf {
    spot_data_dir().join("updates")
}

pub fn updates_archives_dir() -> PathBuf {
    updates_root_dir().join("archives")
}

pub fn updates_manifests_dir() -> PathBuf {
    updates_root_dir().join("manifests")
}

pub fn updates_catalog_path() -> PathBuf {
    updates_root_dir().join("updates_catalog.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedUpdateCatalog {
    pub installed_build_number: u64,
    pub installed_commit_sha: String,
    pub latest_remote: Option<UpdateReleaseManifest>,
    pub stored_updates: Vec<StoredUpdateEntry>,
}

/// Computes the lowercase 64-character hex SHA-256 digest of a byte slice.
pub fn sha256_hex_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

/// Computes the lowercase 64-character hex SHA-256 digest of a file on disk.
pub fn sha256_hex_file(path: &Path) -> Result<String> {
    let bytes = std::fs
        ::read(path)
        .with_context(|| format!("failed to read file for SHA-256: {}", path.display()))?;
    Ok(sha256_hex_bytes(&bytes))
}

/// Loads `UpdateCatalogStore` from `~/Library/Application Support/Spot/updates/updates_catalog.json`,
/// synchronizing the running binary's build/commit and pruning any stored entries whose `.zip`
/// archive is no longer present on disk.
pub fn load_update_catalog() -> UpdateCatalogStore {
    let running_build = compiled_build_number();
    let running_sha = BUILD_COMMIT_SHA;
    let archives_dir = updates_archives_dir();

    let path = updates_catalog_path();
    let persisted: Option<PersistedUpdateCatalog> = std::fs
        ::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok());

    let store = match persisted {
        Some(p) => {
            let valid_on_disk: Vec<StoredUpdateEntry> = p.stored_updates
                .into_iter()
                .filter(|entry| archives_dir.join(&entry.archive_filename).is_file())
                .collect();

            let (inst_build, inst_sha) = if running_build >= p.installed_build_number {
                (running_build, running_sha.to_string())
            } else {
                (p.installed_build_number, p.installed_commit_sha)
            };

            UpdateCatalogStore::from_loaded(
                inst_build,
                &inst_sha,
                p.latest_remote,
                valid_on_disk,
                MAX_STORED_UPDATES
            )
        }
        None => UpdateCatalogStore::new(running_build, running_sha, MAX_STORED_UPDATES),
    };

    debug_assert!(store.check_invariants().is_ok());
    let _ = save_update_catalog(&store);
    store
}

/// Persists `UpdateCatalogStore` to `~/Library/Application Support/Spot/updates/updates_catalog.json`.
pub fn save_update_catalog(store: &UpdateCatalogStore) -> Result<()> {
    let root = updates_root_dir();
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir_all(updates_archives_dir())?;
    std::fs::create_dir_all(updates_manifests_dir())?;

    let payload = PersistedUpdateCatalog {
        installed_build_number: store.installed_build_number(),
        installed_commit_sha: store.installed_commit_sha().to_string(),
        latest_remote: store.latest_remote().cloned(),
        stored_updates: store.stored_updates().to_vec(),
    };
    let json = serde_json::to_string_pretty(&payload)?;
    std::fs::write(updates_catalog_path(), json)?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct GithubReleaseAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Deserialize)]
struct GithubReleaseResponse {
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    body: String,
    #[serde(default)]
    assets: Vec<GithubReleaseAsset>,
}

/// Fetches the latest `main` release manifest from GitHub Releases (`spot-update-manifest.json` asset)
/// or from `SPOT_UPDATE_MANIFEST_URL` if configured.
pub async fn fetch_latest_main_release() -> Result<UpdateReleaseManifest> {
    let client = reqwest::Client
        ::builder()
        .user_agent(format!("Spot/{BUILD_VERSION} ({BUILD_SHORT_SHA})"))
        .build()?;

    if let Ok(custom_url) = std::env::var("SPOT_UPDATE_MANIFEST_URL") {
        let trimmed = custom_url.trim();
        if !trimmed.is_empty() {
            if let Some(local_path) = trimmed.strip_prefix("file://") {
                let raw = std::fs::read_to_string(local_path)?;
                let parsed: UpdateReleaseManifest = serde_json::from_str(&raw)?;
                return parsed
                    .sanitize()
                    .ok_or_else(|| anyhow!("invalid update manifest at {local_path}"));
            } else if Path::new(trimmed).is_file() {
                let raw = std::fs::read_to_string(trimmed)?;
                let parsed: UpdateReleaseManifest = serde_json::from_str(&raw)?;
                return parsed
                    .sanitize()
                    .ok_or_else(|| anyhow!("invalid update manifest at {trimmed}"));
            }
            let resp = client.get(trimmed).send().await?.error_for_status()?;
            let parsed: UpdateReleaseManifest = resp.json().await?;
            return parsed
                .sanitize()
                .ok_or_else(|| anyhow!("invalid update manifest from {trimmed}"));
        }
    }

    let repo = default_github_repo();
    let api_url = format!("https://api.github.com/repos/{repo}/releases?per_page=10");
    let mut req = client.get(&api_url).header("Accept", "application/vnd.github+json");

    if
        let Ok(token) = std::env
            ::var("GITHUB_TOKEN")
            .or_else(|_| std::env::var("GH_TOKEN"))
            .or_else(|_| std::env::var("SPOT_GITHUB_TOKEN"))
    {
        if !token.trim().is_empty() {
            req = req.bearer_auth(token.trim());
        }
    }

    let resp = req
        .send().await
        .with_context(|| format!("failed to query GitHub releases for {repo}"))?;

    if !resp.status().is_success() {
        bail!("GitHub Releases API returned HTTP {} for {}", resp.status(), repo);
    }

    let releases: Vec<GithubReleaseResponse> = resp.json().await?;
    for release in releases {
        if release.draft {
            continue;
        }
        if
            let Some(manifest_asset) = release.assets
                .iter()
                .find(|a| a.name == "spot-update-manifest.json")
        {
            let m_resp = client
                .get(&manifest_asset.browser_download_url)
                .send().await?
                .error_for_status()?;
            let mut manifest: UpdateReleaseManifest = m_resp.json().await?;
            if manifest.release_notes.trim().is_empty() && !release.body.trim().is_empty() {
                manifest.release_notes = release.body.clone();
            }
            if let Some(clean) = manifest.sanitize() {
                return Ok(clean);
            }
        }
    }

    bail!(
        "No published main update releases found on https://github.com/{repo}/releases yet"
    )
}

/// Downloads the release archive specified by `manifest`, verifies its SHA-256 digest (`INV-U2`),
/// stores it in `~/Library/Application Support/Spot/updates/archives/Spot-b<build>-<short_sha>.zip`,
/// prunes evicted archives beyond `MAX_STORED_UPDATES`, and updates `updates_catalog.json`.
pub async fn download_and_store_update(
    manifest: UpdateReleaseManifest
) -> Result<(StoredUpdateEntry, PathBuf)> {
    let manifest = manifest
        .sanitize()
        .ok_or_else(|| anyhow!("cannot download malformed UpdateReleaseManifest"))?;

    let archives_dir = updates_archives_dir();
    let manifests_dir = updates_manifests_dir();
    std::fs::create_dir_all(&archives_dir)?;
    std::fs::create_dir_all(&manifests_dir)?;

    let archive_filename = manifest.canonical_archive_filename();
    let archive_path = archives_dir.join(&archive_filename);

    // If the archive is already on disk with a verified SHA-256, reuse it directly.
    let actual_sha256 = if archive_path.is_file() {
        let existing_hash = sha256_hex_file(&archive_path)?;
        if verify_update_digest_verified(&manifest.sha256, &existing_hash) {
            existing_hash
        } else {
            let _ = std::fs::remove_file(&archive_path);
            download_archive_bytes(&manifest.download_url, &archive_path).await?
        }
    } else {
        download_archive_bytes(&manifest.download_url, &archive_path).await?
    };

    let mut store = load_update_catalog();
    let (entry, evicted) = store
        .record_downloaded_update(manifest.clone(), &actual_sha256)
        .map_err(|v| {
            let _ = std::fs::remove_file(&archive_path);
            anyhow!("[{}] {}: {}", v.code, v.subsystem, v.details)
        })?;

    // Save the standalone manifest JSON next to the archive in `updates/manifests/`
    let manifest_stem = archive_filename.trim_end_matches(".zip");
    let manifest_file_path = manifests_dir.join(format!("{manifest_stem}.json"));
    let _ = std::fs::write(&manifest_file_path, serde_json::to_string_pretty(&manifest)?);

    // Remove any old archives evicted by the bounded MRU store (`INV-U3`)
    for evicted_name in evicted {
        let _ = std::fs::remove_file(archives_dir.join(&evicted_name));
        let stem = evicted_name.trim_end_matches(".zip");
        let _ = std::fs::remove_file(manifests_dir.join(format!("{stem}.json")));
    }

    debug_assert!(store.check_invariants().is_ok());
    save_update_catalog(&store)?;

    Ok((entry, archive_path))
}

async fn download_archive_bytes(url: &str, dest_path: &Path) -> Result<String> {
    let bytes = if let Some(local_src) = url.strip_prefix("file://") {
        std::fs::read(local_src)?
    } else if Path::new(url).is_file() {
        std::fs::read(url)?
    } else {
        let client = reqwest::Client
            ::builder()
            .user_agent(format!("Spot/{BUILD_VERSION} ({BUILD_SHORT_SHA})"))
            .build()?;
        let resp = client
            .get(url)
            .send().await
            .with_context(|| format!("failed to download update archive from {url}"))?
            .error_for_status()?;
        resp.bytes().await?.to_vec()
    };

    let actual_sha256 = sha256_hex_bytes(&bytes);
    let tmp_path = dest_path.with_extension("zip.part");
    std::fs::write(&tmp_path, &bytes)?;
    std::fs::rename(&tmp_path, dest_path)?;
    Ok(actual_sha256)
}

/// Verifies and installs a stored update archive (`Spot-b<build>-<short_sha>.zip`) into
/// `/Applications/Spot.app` (or `SPOT_INSTALL_DIR`) and updates `~/.local/bin/spot`.
pub fn install_stored_update_entry(entry: &StoredUpdateEntry) -> Result<PathBuf> {
    let archive_path = updates_archives_dir().join(&entry.archive_filename);
    if !archive_path.is_file() {
        bail!("Stored update archive not found at {}", archive_path.display());
    }

    let disk_sha256 = sha256_hex_file(&archive_path)?;
    if !verify_update_digest_verified(&entry.manifest.sha256, &disk_sha256) {
        bail!(
            "[INV-U2] Refusing to install {}: SHA-256 on disk ({}) does not match expected {}",
            entry.archive_filename,
            disk_sha256,
            entry.manifest.sha256
        );
    }

    let staging_dir = updates_root_dir().join("staging");
    if staging_dir.exists() {
        let _ = std::fs::remove_dir_all(&staging_dir);
    }
    std::fs::create_dir_all(&staging_dir)?;

    let unzip_status = std::process::Command
        ::new("/usr/bin/ditto")
        .args([
            "-x",
            "-k",
            archive_path.to_str().unwrap_or_default(),
            staging_dir.to_str().unwrap_or_default(),
        ])
        .status()
        .context("failed to invoke /usr/bin/ditto to extract update archive")?;

    if !unzip_status.success() {
        let _ = std::fs::remove_dir_all(&staging_dir);
        bail!("failed to extract {} with ditto", archive_path.display());
    }

    let staged_app = staging_dir.join("Spot.app");
    if !staged_app.join("Contents/MacOS/spot").is_file() {
        let _ = std::fs::remove_dir_all(&staging_dir);
        bail!("extracted archive did not contain a valid Spot.app bundle");
    }

    let preferred_root = std::env
        ::var("SPOT_INSTALL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/Applications"));

    let install_root = if std::fs::create_dir_all(&preferred_root).is_ok() {
        preferred_root
    } else {
        let user_apps = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Applications");
        std::fs::create_dir_all(&user_apps)?;
        user_apps
    };
    let mut target_app = install_root.join("Spot.app");

    if target_app.exists() && std::fs::remove_dir_all(&target_app).is_err() {
        let user_apps = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Applications");
        std::fs::create_dir_all(&user_apps)?;
        target_app = user_apps.join("Spot.app");
        if target_app.exists() {
            std::fs
                ::remove_dir_all(&target_app)
                .with_context(|| format!("failed to replace {}", target_app.display()))?;
        }
    }

    let copy_status = std::process::Command
        ::new("/usr/bin/ditto")
        .args([
            staged_app.to_str().unwrap_or_default(),
            target_app.to_str().unwrap_or_default(),
        ])
        .status()
        .context("failed to copy staged Spot.app to install directory")?;

    let _ = std::fs::remove_dir_all(&staging_dir);
    if !copy_status.success() {
        bail!("failed to install Spot.app to {}", target_app.display());
    }

    let installed_bin = target_app.join("Contents/MacOS/spot");

    // Update ~/.local/bin/spot (or SPOT_BIN_DIR) symlink
    let bin_dir = std::env
        ::var("SPOT_BIN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/bin")
        });
    if std::fs::create_dir_all(&bin_dir).is_ok() {
        let link_path = bin_dir.join("spot");
        let _ = std::fs::remove_file(&link_path);
        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink(
                &installed_bin,
                &link_path
            );
        }
    }

    // If the currently running binary is a standalone executable (e.g. ./target/debug/spot or ./target/release/spot)
    // and is writable, atomically replace it via a staged rename so subsequent direct invocations also run the new build.
    if !cfg!(test) && std::env::var("SPOT_TEST_CACHE_DIR").is_err() {
        if let Ok(cur_exe) = std::env::current_exe() {
            if cur_exe != installed_bin
                && cur_exe
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n == "spot")
            {
                let tmp_exe = cur_exe.with_extension("spot-update-new");
                if std::fs::copy(&installed_bin, &tmp_exe).is_ok() {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = std::fs::set_permissions(
                            &tmp_exe,
                            std::fs::Permissions::from_mode(0o755),
                        );
                    }
                    let _ = std::fs::rename(&tmp_exe, &cur_exe);
                }
            }
        }
    }

    let mut store = load_update_catalog();
    store.mark_installed(entry.manifest.build_number, &entry.manifest.commit_sha);
    debug_assert!(store.check_invariants().is_ok());
    save_update_catalog(&store)?;

    Ok(target_app)
}

/// Spawns a fresh instance of the newly installed `Spot` binary after a brief 180ms handoff
/// and terminates the current process so the updated build takes effect immediately.
pub fn relaunch_spot_after_update(installed_app: Option<&Path>) -> Result<()> {
    let candidate_bin = installed_app
        .map(|app| {
            if app.ends_with("Spot.app") {
                app.join("Contents/MacOS/spot")
            } else {
                app.to_path_buf()
            }
        })
        .filter(|p| p.is_file())
        .or_else(|| {
            let default_app_bin = PathBuf::from("/Applications/Spot.app/Contents/MacOS/spot");
            if default_app_bin.is_file() {
                Some(default_app_bin)
            } else {
                std::env::current_exe().ok()
            }
        })
        .ok_or_else(|| anyhow!("could not locate updated Spot binary to relaunch"))?;

    tracing::info!(
        "[Updater] Relaunching Spot into updated binary at {}",
        candidate_bin.display()
    );

    if cfg!(test) || std::env::var("SPOT_TEST_CACHE_DIR").is_ok() {
        return Ok(());
    }

    let bin_str = candidate_bin
        .to_str()
        .ok_or_else(|| anyhow!("invalid UTF-8 in binary path"))?;

    std::process::Command::new("/bin/sh")
        .args(["-c", "sleep 0.18; exec \"$1\"", "--", bin_str])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn updated Spot binary at {bin_str}"))?;

    std::process::exit(0);
}

/// Handles CLI update flags (`--version`, `--check-update`, `--download-update`, `--update`,
/// `--list-updates`, `--rollback <sha_or_build>`).
/// Returns `Ok(true)` if a CLI command was handled and the process should exit.
pub fn handle_cli_update_args(args: &[String]) -> Result<bool> {
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!(
            "Spot {} (build #{}, commit {}, built {})",
            BUILD_VERSION,
            compiled_build_number(),
            BUILD_SHORT_SHA,
            BUILD_TIMESTAMP
        );
        return Ok(true);
    }

    if args.iter().any(|a| a == "--list-updates") {
        let store = load_update_catalog();
        println!(
            "Installed: Spot {} (build #{}, commit {})",
            BUILD_VERSION,
            store.installed_build_number(),
            store.installed_short_sha()
        );
        println!("Update Store: {}", updates_archives_dir().display());
        if store.stored_updates().is_empty() {
            println!("No downloaded updates stored locally yet.");
        } else {
            println!(
                "Stored updates ({}/{}):",
                store.stored_updates().len(),
                store.max_stored()
            );
            for entry in store.stored_updates() {
                let active_marker = if
                    git_shas_match_verified(
                        &entry.manifest.commit_sha,
                        store.installed_commit_sha()
                    )
                {
                    " [installed]"
                } else {
                    ""
                };
                println!(
                    "  • build #{} ({}) — {} [{}]{}",
                    entry.manifest.build_number,
                    entry.manifest.short_sha,
                    entry.manifest.commit_title,
                    entry.archive_filename,
                    active_marker
                );
            }
        }
        return Ok(true);
    }

    if let Some(pos) = args.iter().position(|a| a == "--rollback") {
        let target = args
            .get(pos + 1)
            .ok_or_else(|| anyhow!("Usage: spot --rollback <commit_sha_or_build_number>"))?;
        let store = load_update_catalog();
        let trimmed = target.trim().trim_start_matches('#').trim_start_matches('b');
        let matched = store
            .stored_updates()
            .iter()
            .find(|e| {
                git_shas_match_verified(&e.manifest.commit_sha, trimmed) ||
                    Ok(e.manifest.build_number) == trimmed.parse::<u64>()
            })
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "No stored update matching '{}' in {}",
                    target,
                    updates_archives_dir().display()
                )
            })?;

        let installed_path = install_stored_update_entry(&matched)?;
        println!(
            "Installed stored build #{} ({}) to {}",
            matched.manifest.build_number,
            matched.manifest.short_sha,
            installed_path.display()
        );
        return Ok(true);
    }

    let check_only = args.iter().any(|a| a == "--check-update");
    let download_only = args.iter().any(|a| a == "--download-update");
    let do_update = args.iter().any(|a| a == "--update");

    if !check_only && !download_only && !do_update {
        return Ok(false);
    }

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async move {
        let mut store = load_update_catalog();
        println!(
            "Current Spot build: #{} (commit {})",
            store.installed_build_number(),
            store.installed_short_sha()
        );
        let remote = fetch_latest_main_release().await?;
        store.observe_remote_release(remote.clone());
        save_update_catalog(&store)?;

        match store.evaluate_availability() {
            UpdateAvailability::UpToDate { installed_build, installed_short_sha } => {
                println!(
                    "Spot is up to date with main (build #{installed_build}, commit {installed_short_sha})."
                );
                if download_only {
                    let (entry, path) = download_and_store_update(remote).await?;
                    println!(
                        "Stored verified archive for build #{} ({}) at {}",
                        entry.manifest.build_number,
                        entry.manifest.short_sha,
                        path.display()
                    );
                }
            }
            UpdateAvailability::UpdateAvailable { manifest, stored_archive_filename } => {
                println!(
                    "New main update available: build #{} ({}) — {}",
                    manifest.build_number,
                    manifest.short_sha,
                    manifest.commit_title
                );
                if check_only {
                    if let Some(filename) = stored_archive_filename {
                        println!(
                            "Already downloaded in store: {}",
                            updates_archives_dir().join(filename).display()
                        );
                    } else {
                        println!("Run `spot --update` or `make update` to download and install.");
                    }
                    return Ok(());
                }

                println!("Downloading {} ({} bytes)...", manifest.asset_name, manifest.size_bytes);
                let (entry, path) = download_and_store_update(manifest).await?;
                println!(
                    "Verified SHA-256 ({}) & stored at {}",
                    &entry.verified_sha256[..12],
                    path.display()
                );

                if do_update {
                    let installed_path = install_stored_update_entry(&entry)?;
                    println!(
                        "Updated Spot to build #{} ({}) at {}",
                        entry.manifest.build_number,
                        entry.manifest.short_sha,
                        installed_path.display()
                    );
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    })?;

    Ok(true)
}
