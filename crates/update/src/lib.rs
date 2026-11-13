//! In-app updates sourced from GitHub Releases.
//!
//! The flow is deliberately small and auditable:
//!
//! 1. Ask the GitHub Releases API for the newest published release.
//! 2. Pick the asset that matches this machine and compare versions.
//! 3. Download the archive and its `checksums.txt`, verify SHA-256, extract
//!    into a staging directory, and validate the payload.
//! 4. Hand the staged update to a small platform helper. The helper waits for
//!    this process to exit, swaps the app, and relaunches it.
//!
//! macOS releases are `.app` bundles. Windows and Linux releases are portable
//! `Shortlist/` folders carrying `shortlist-install.json`.
//!
//! Candidate data never leaves the machine; only the update check talks to
//! GitHub, and it can be turned off in Settings.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Repository that publishes Shortlist releases. Forks can override this at
/// build time with `SHORTLIST_UPDATE_REPO=owner/repo`.
pub const DEFAULT_REPO: &str = "alok-mishra143/shortlist";

/// Bundle identifier written by `scripts/package-macos.sh`. An update is only
/// installed if the downloaded bundle carries the same identifier.
pub const APP_BUNDLE_ID: &str = "com.shortlist.app";

/// Marker written into Windows and Linux portable builds.
pub const INSTALL_MARKER: &str = "shortlist-install.json";

/// Application id inside [`INSTALL_MARKER`].
pub const APP_MARKER_ID: &str = "shortlist";

/// Top-level directory inside Windows and Linux archives.
pub const APP_DIR_NAME: &str = "Shortlist";

/// Name of the checksum manifest attached to every release.
pub const CHECKSUMS_ASSET: &str = "checksums.txt";

/// Refuse absurd archives before touching the filesystem.
const MAX_ARCHIVE_ENTRIES: usize = 20_000;
const MAX_EXTRACTED_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug)]
pub enum CheckResult {
    /// The installed version is current.
    UpToDate { current: Version, latest: Version },
    /// A newer, installable release is available.
    Available(ReleaseInfo),
}

#[derive(Clone, Debug)]
pub struct ReleaseInfo {
    pub version: Version,
    pub tag: String,
    pub title: String,
    pub notes: String,
    pub page_url: String,
    pub published_at: Option<String>,
    /// `None` when the release has no build for this platform or architecture.
    pub asset: Option<ReleaseAsset>,
    /// `None` when the release has no checksum manifest. Such a release is
    /// viewable but never auto-installed.
    pub checksums_url: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ReleaseAsset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// How the app was installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// `Shortlist.app` on macOS.
    MacBundle,
    /// A portable `Shortlist/` folder on Windows or Linux.
    Directory,
}

/// The currently running install.
#[derive(Clone, Debug)]
pub struct Installation {
    pub kind: InstallKind,
    /// The `.app` bundle or portable app directory.
    pub root: PathBuf,
}

/// A verified, extracted release waiting to replace the running app.
#[derive(Clone, Debug)]
pub struct StagedUpdate {
    pub version: Version,
    pub kind: InstallKind,
    /// The staged `.app` bundle or portable app directory.
    pub payload: PathBuf,
    pub work_dir: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct InstallManifest {
    app: String,
    version: String,
    platform: String,
    executable: String,
}

/// The version compiled into this binary.
pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("workspace version is valid semver")
}

/// The repository used for update checks.
pub fn repo() -> String {
    option_env!("SHORTLIST_UPDATE_REPO")
        .map(str::trim)
        .filter(|repo| !repo.is_empty())
        .unwrap_or(DEFAULT_REPO)
        .to_string()
}

/// Query GitHub for the newest published release.
pub fn check_for_update() -> Result<CheckResult> {
    check_for_update_for(
        &repo(),
        &current_version(),
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

fn check_for_update_for(
    repo: &str,
    current: &Version,
    os: &str,
    arch: &str,
) -> Result<CheckResult> {
    let response = client(Duration::from_secs(30))?
        .get(format!(
            "https://api.github.com/repos/{repo}/releases/latest"
        ))
        .header("Accept", "application/vnd.github+json")
        .send()
        .context("Could not reach GitHub to check for updates.")?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("No published releases found for {repo}.");
    }

    let response = response
        .error_for_status()
        .context("GitHub returned an error while checking for updates.")?;
    let raw: GithubRelease = response
        .json()
        .context("GitHub sent an update response this build could not read.")?;

    if raw.draft || raw.prerelease {
        bail!("The newest GitHub release is still a draft or prerelease.");
    }

    release_from_github(raw, current, os, arch)
}

fn release_from_github(
    raw: GithubRelease,
    current: &Version,
    os: &str,
    arch: &str,
) -> Result<CheckResult> {
    let latest = parse_tag_version(&raw.tag_name)?;
    if latest <= *current {
        return Ok(CheckResult::UpToDate {
            current: current.clone(),
            latest,
        });
    }

    let expected = expected_asset_name(os, arch);
    let asset = expected.as_ref().and_then(|name| {
        raw.assets
            .iter()
            .find(|asset| asset.name == *name)
            .map(|asset| ReleaseAsset {
                name: asset.name.clone(),
                url: asset.browser_download_url.clone(),
                size: asset.size,
            })
    });
    let checksums_url = raw
        .assets
        .iter()
        .find(|asset| asset.name == CHECKSUMS_ASSET)
        .map(|asset| asset.browser_download_url.clone());

    Ok(CheckResult::Available(ReleaseInfo {
        version: latest,
        tag: raw.tag_name,
        title: raw.name.unwrap_or_default(),
        notes: raw.body.unwrap_or_default(),
        page_url: raw.html_url,
        published_at: raw.published_at,
        asset,
        checksums_url,
    }))
}

pub fn parse_tag_version(tag: &str) -> Result<Version> {
    let trimmed = tag.trim().trim_start_matches(['v', 'V']);
    Version::parse(trimmed)
        .with_context(|| format!("Release tag \"{tag}\" is not a semantic version."))
}

/// Asset name produced by the packaging scripts.
pub fn expected_asset_name(os: &str, arch: &str) -> Option<String> {
    match (os, arch) {
        ("macos", "aarch64" | "x86_64") => Some(format!("Shortlist-macos-{arch}.zip")),
        ("windows", "aarch64" | "x86_64") => Some(format!("Shortlist-windows-{arch}.zip")),
        ("linux", "aarch64" | "x86_64") => Some(format!("Shortlist-linux-{arch}.zip")),
        _ => None,
    }
}

/// Download, verify, and extract a release into a staging directory.
///
/// `on_progress` receives `(bytes_received, total_bytes)`. The caller may flip
/// `cancel` from another thread; the download then stops with an error.
pub fn download_and_stage(
    release: &ReleaseInfo,
    work_root: &Path,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<StagedUpdate> {
    let asset = release.asset.as_ref().ok_or_else(|| {
        anyhow!(
            "This release has no build for {} {}. Download it from {} instead.",
            std::env::consts::OS,
            std::env::consts::ARCH,
            release.page_url
        )
    })?;
    let checksums_url = release.checksums_url.as_ref().ok_or_else(|| {
        anyhow!(
            "This release does not include {CHECKSUMS_ASSET}, so the download cannot be verified. Install it manually from {}.",
            release.page_url
        )
    })?;

    validate_download_url(&asset.url)?;
    validate_download_url(checksums_url)?;

    let work_root = absolute_path(work_root)?;
    let work_dir = work_root.join(format!("v{}-{}", release.version, std::process::id()));
    if work_dir.exists() {
        let _ = fs::remove_dir_all(&work_dir);
    }
    fs::create_dir_all(&work_dir)
        .with_context(|| format!("Could not create {}.", work_dir.display()))?;

    let staged = stage_release(
        release,
        asset,
        checksums_url,
        &work_dir,
        cancel,
        on_progress,
    );
    if staged.is_err() {
        let _ = fs::remove_dir_all(&work_dir);
    }
    staged
}

fn stage_release(
    release: &ReleaseInfo,
    asset: &ReleaseAsset,
    checksums_url: &str,
    work_dir: &Path,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<StagedUpdate> {
    let client = client(Duration::from_secs(600))?;

    let checksums = fetch_checksums(&client, checksums_url)?;
    let expected = checksums.get(&asset.name).ok_or_else(|| {
        anyhow!(
            "{CHECKSUMS_ASSET} does not list {}. The release is incomplete.",
            asset.name
        )
    })?;

    let archive_path = work_dir.join("Shortlist.zip");
    let actual = download_file(
        &client,
        &asset.url,
        &archive_path,
        asset.size,
        cancel,
        on_progress,
    )?;
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("The downloaded update failed its checksum and was not installed.");
    }

    let extract_dir = work_dir.join("extracted");
    fs::create_dir_all(&extract_dir)
        .with_context(|| format!("Could not create {}.", extract_dir.display()))?;
    extract_zip(&archive_path, &extract_dir)?;
    let _ = fs::remove_file(&archive_path);

    let (kind, payload) = find_staged_payload(&extract_dir, &release.version)?;

    Ok(StagedUpdate {
        version: release.version.clone(),
        kind,
        payload,
        work_dir: work_dir.to_path_buf(),
    })
}

fn fetch_checksums(
    client: &reqwest::blocking::Client,
    checksums_url: &str,
) -> Result<HashMap<String, String>> {
    let text = client
        .get(checksums_url)
        .send()
        .context("Could not download the update checksums.")?
        .error_for_status()
        .context("GitHub returned an error while downloading update checksums.")?
        .text()
        .context("Could not read the update checksums.")?;

    let checksums = parse_checksums(&text);
    if checksums.is_empty() {
        bail!("The release checksum file is empty or malformed.");
    }
    Ok(checksums)
}

fn parse_checksums(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let name = parts.next()?.trim_start_matches('*');
            if hash.len() != 64 || !hash.chars().all(|ch| ch.is_ascii_hexdigit()) {
                return None;
            }
            Some((name.to_string(), hash.to_ascii_lowercase()))
        })
        .collect()
}

fn download_file(
    client: &reqwest::blocking::Client,
    url: &str,
    destination: &Path,
    size_hint: u64,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<String> {
    let mut response = client
        .get(url)
        .send()
        .context("Could not download the update.")?
        .error_for_status()
        .context("GitHub returned an error while downloading the update.")?;

    let total = response
        .content_length()
        .filter(|length| *length > 0)
        .or((size_hint > 0).then_some(size_hint));
    let mut file = File::create(destination)
        .with_context(|| format!("Could not write {}.", destination.display()))?;
    let mut hasher = Sha256::new();
    let mut received = 0u64;
    let mut buffer = vec![0u8; 64 * 1024];

    on_progress(0, total);
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("Update cancelled.");
        }
        let read = response
            .read(&mut buffer)
            .context("The update download was interrupted.")?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .context("Could not write the update download.")?;
        hasher.update(&buffer[..read]);
        received += read as u64;
        on_progress(received, total);
    }
    file.sync_all().ok();

    if received == 0 {
        bail!("The update download was empty.");
    }
    if let Some(total) = total
        && received != total
    {
        bail!("The update download stopped early ({received} of {total} bytes).");
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn extract_zip(archive_path: &Path, destination: &Path) -> Result<()> {
    let file = File::open(archive_path)
        .with_context(|| format!("Could not open {}.", archive_path.display()))?;
    let mut archive =
        zip::ZipArchive::new(file).context("The update archive could not be opened.")?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        bail!("The update archive contains too many files.");
    }

    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .context("The update archive could not be read.")?;
        let Some(relative) = entry.enclosed_name() else {
            bail!("The update archive contains an unsafe path.");
        };
        if relative.as_os_str().is_empty() || relative.starts_with("__MACOSX") {
            continue;
        }

        let is_symlink = entry
            .unix_mode()
            .map(|mode| mode & 0o170000 == 0o120000)
            .unwrap_or(false);
        if is_symlink {
            bail!("The update archive contains a symbolic link, which Shortlist does not install.");
        }

        let output = destination.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&output)
                .with_context(|| format!("Could not create {}.", output.display()))?;
            set_mode(&output, entry.unix_mode());
            continue;
        }

        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Could not create {}.", parent.display()))?;
        }

        let mut out = File::create(&output)
            .with_context(|| format!("Could not write {}.", output.display()))?;
        let copied = io::copy(&mut entry, &mut out)
            .with_context(|| format!("Could not write {}.", output.display()))?;
        total = total.saturating_add(copied);
        if total > MAX_EXTRACTED_BYTES {
            bail!("The update archive is unexpectedly large.");
        }
        set_mode(&output, entry.unix_mode());
    }
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: Option<u32>) {
    use std::os::unix::fs::PermissionsExt;
    if let Some(mode) = mode {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777));
    }
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: Option<u32>) {}

fn find_staged_payload(
    directory: &Path,
    expected_version: &Version,
) -> Result<(InstallKind, PathBuf)> {
    if std::env::consts::OS == "macos" {
        let bundle = find_mac_bundle(directory)?;
        validate_mac_bundle(&bundle, expected_version)?;
        return Ok((InstallKind::MacBundle, bundle));
    }

    let app_dir = directory.join(APP_DIR_NAME);
    if !app_dir.is_dir() {
        bail!("The update archive did not contain a {APP_DIR_NAME} folder.");
    }
    validate_directory_install(&app_dir, expected_version)?;
    Ok((InstallKind::Directory, app_dir))
}

fn find_mac_bundle(directory: &Path) -> Result<PathBuf> {
    let mut bundles = Vec::new();
    for entry in fs::read_dir(directory)
        .with_context(|| format!("Could not read {}.", directory.display()))?
    {
        let path = entry?.path();
        let hidden = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.starts_with('.'))
            .unwrap_or(true);
        if !hidden && path.is_dir() && path.extension().and_then(|ext| ext.to_str()) == Some("app")
        {
            bundles.push(path);
        }
    }

    match bundles.len() {
        1 => Ok(bundles.remove(0)),
        0 => bail!("The update archive did not contain an app bundle."),
        _ => bail!("The update archive contained more than one app bundle."),
    }
}

fn validate_mac_bundle(bundle: &Path, expected_version: &Version) -> Result<()> {
    let executable = bundle.join("Contents/MacOS/shortlist");
    if !executable.is_file() {
        bail!("The downloaded app is missing its main executable.");
    }

    let plist = bundle.join("Contents/Info.plist");
    let plist = fs::read_to_string(&plist)
        .with_context(|| format!("The downloaded app is missing {}.", plist.display()))?;
    if !plist.contains(APP_BUNDLE_ID) {
        bail!("The downloaded app is not Shortlist, so it was not installed.");
    }
    if !plist.contains(&format!("<string>{expected_version}</string>")) {
        bail!(
            "The downloaded app reports a different version than the release. Nothing was installed."
        );
    }
    Ok(())
}

fn validate_directory_install(directory: &Path, expected_version: &Version) -> Result<()> {
    let manifest = read_install_manifest(&directory.join(INSTALL_MARKER))?;
    if manifest.app != APP_MARKER_ID {
        bail!("The downloaded app is not Shortlist, so it was not installed.");
    }
    if manifest.platform != std::env::consts::OS {
        bail!(
            "The downloaded update is for {}, not {}.",
            manifest.platform,
            std::env::consts::OS
        );
    }
    if manifest.version != expected_version.to_string() {
        bail!(
            "The downloaded app reports a different version than the release. Nothing was installed."
        );
    }
    if manifest.executable != platform_executable() {
        bail!("The downloaded app has an unexpected executable name. Nothing was installed.");
    }
    if !directory.join(platform_executable()).is_file() {
        bail!("The downloaded app is missing its main executable.");
    }
    if !directory.join(platform_pdf_helper()).is_file() {
        bail!("The downloaded app is missing its PDF helper.");
    }
    Ok(())
}

fn read_install_manifest(path: &Path) -> Result<InstallManifest> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("The downloaded app is missing {}.", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("Could not read {}.", path.display()))
}

fn platform_executable() -> &'static str {
    if cfg!(windows) {
        "shortlist.exe"
    } else {
        "shortlist"
    }
}

fn platform_pdf_helper() -> &'static str {
    if cfg!(windows) {
        "shortlist-pdf.exe"
    } else {
        "shortlist-pdf"
    }
}

/// The current packaged installation, or `None` for a development build.
pub fn current_installation() -> Option<Installation> {
    let executable = std::env::current_exe().ok()?;

    if cfg!(target_os = "macos") {
        for ancestor in executable.ancestors() {
            if ancestor.extension().and_then(|ext| ext.to_str()) == Some("app") {
                let plist = fs::read_to_string(ancestor.join("Contents/Info.plist")).ok()?;
                if plist.contains(APP_BUNDLE_ID) {
                    return Some(Installation {
                        kind: InstallKind::MacBundle,
                        root: ancestor.to_path_buf(),
                    });
                }
            }
        }
        return None;
    }

    directory_installation(&executable, std::env::consts::OS)
}

fn directory_installation(executable: &Path, os: &str) -> Option<Installation> {
    let root = executable.parent()?.to_path_buf();
    let manifest = read_install_manifest(&root.join(INSTALL_MARKER)).ok()?;
    if manifest.app != APP_MARKER_ID || manifest.platform != os {
        return None;
    }
    Some(Installation {
        kind: InstallKind::Directory,
        root,
    })
}

/// Whether this process is running from a packaged, self-updatable install.
pub fn is_packaged_install() -> bool {
    current_installation().is_some()
}

/// Replace the running app with a staged update and relaunch it.
///
/// Returns after the detached helper process has been started. The caller must
/// quit the app so the helper can swap the app. The helper restores the old
/// app if copying fails and logs everything to `<data>/logs/update.log`.
pub fn install_and_restart(staged: &StagedUpdate, data_dir: &Path) -> Result<()> {
    let installation = current_installation().ok_or_else(|| {
        anyhow!(
            "Shortlist is running from a development build, so it cannot replace itself. Download the release from GitHub instead."
        )
    })?;

    let data_dir = absolute_path(data_dir)?;
    let payload = absolute_path(&staged.payload)?;
    let work_dir = absolute_path(&staged.work_dir)?;

    if installation.kind != staged.kind {
        bail!("The downloaded update does not match this installation. Nothing was installed.");
    }
    if !payload.is_dir() {
        bail!("The downloaded update is incomplete. Try again.");
    }
    if installation.root == payload {
        bail!("The update was staged over the running app. Nothing was changed.");
    }
    if installation.kind == InstallKind::MacBundle
        && installation
            .root
            .to_string_lossy()
            .contains("AppTranslocation")
    {
        bail!(
            "macOS is running Shortlist from a read-only temporary location. Move Shortlist to Applications and try again."
        );
    }

    let parent = installation
        .root
        .parent()
        .ok_or_else(|| anyhow!("Could not locate the folder that contains Shortlist."))?;
    probe_writable(parent)?;

    let log_dir = data_dir.join("logs");
    fs::create_dir_all(&log_dir)
        .with_context(|| format!("Could not create {}.", log_dir.display()))?;
    let log_path = log_dir.join("update.log");

    match installation.kind {
        InstallKind::MacBundle => {
            install_mac_bundle(&installation.root, &payload, &work_dir, &log_path)
        }
        InstallKind::Directory => {
            install_directory(&installation.root, &payload, &work_dir, &log_path)
        }
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .context("Could not read the current directory.")?
            .join(path))
    }
}

fn install_mac_bundle(
    target: &Path,
    staged: &Path,
    work_dir: &Path,
    log_path: &Path,
) -> Result<()> {
    let script_path = work_dir.join("install.sh");
    fs::write(&script_path, MAC_HELPER_SCRIPT)
        .with_context(|| format!("Could not write {}.", script_path.display()))?;
    set_executable(&script_path)?;

    Command::new("/bin/sh")
        .arg(&script_path)
        .arg(std::process::id().to_string())
        .arg(target)
        .arg(staged)
        .arg(log_path)
        .arg(work_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Could not start the update helper.")?;

    Ok(())
}

fn install_directory(target: &Path, staged: &Path, work_dir: &Path, log_path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        install_windows_directory(target, staged, work_dir, log_path)
    }
    #[cfg(not(windows))]
    {
        install_unix_directory(target, staged, work_dir, log_path)
    }
}

#[cfg(not(windows))]
fn install_unix_directory(
    target: &Path,
    staged: &Path,
    work_dir: &Path,
    log_path: &Path,
) -> Result<()> {
    let script_path = work_dir.join("install.sh");
    fs::write(&script_path, UNIX_HELPER_SCRIPT)
        .with_context(|| format!("Could not write {}.", script_path.display()))?;
    set_executable(&script_path)?;

    Command::new("/bin/sh")
        .arg(&script_path)
        .arg(std::process::id().to_string())
        .arg(target)
        .arg(staged)
        .arg(log_path)
        .arg(work_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Could not start the update helper.")?;

    Ok(())
}

#[cfg(windows)]
fn install_windows_directory(
    target: &Path,
    staged: &Path,
    work_dir: &Path,
    log_path: &Path,
) -> Result<()> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let script_path = work_dir.join("install.ps1");
    fs::write(&script_path, WINDOWS_HELPER_SCRIPT)
        .with_context(|| format!("Could not write {}.", script_path.display()))?;

    let powershell = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .map(|root| root.join("System32/WindowsPowerShell/v1.0/powershell.exe"))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("powershell.exe"));

    let mut command = Command::new(powershell);
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-File",
        ])
        .arg(&script_path)
        .arg("-AppPid")
        .arg(std::process::id().to_string())
        .arg("-Target")
        .arg(target)
        .arg("-Staged")
        .arg(staged)
        .arg("-Log")
        .arg(log_path)
        .arg("-Work")
        .arg(work_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    command
        .spawn()
        .context("Could not start the update helper.")?;

    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .context("Could not prepare the update helper.")
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

fn probe_writable(directory: &Path) -> Result<()> {
    let probe = directory.join(format!(".shortlist-update-{}", std::process::id()));
    match File::create(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(error) => bail!(
            "Shortlist cannot replace itself in {}: {error}. Move the app somewhere writable or install the new release manually.",
            directory.display()
        ),
    }
}

fn validate_download_url(url: &str) -> Result<()> {
    let prefix = format!("https://github.com/{}/releases/download/", repo());
    if !url.starts_with(&prefix) {
        bail!("The update points at an unexpected download location. Nothing was installed.");
    }
    Ok(())
}

fn client(timeout: Duration) -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("Shortlist-App/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout)
        .build()
        .context("Could not create the update client.")
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// Detached macOS helper. It must stay dependency-free: the only inputs are
/// the arguments passed by `install_and_restart`, and every path is quoted.
const MAC_HELPER_SCRIPT: &str = r#"#!/bin/sh
# Shortlist auto-update helper: wait for the app to quit, swap the bundle,
# clear Gatekeeper quarantine, then relaunch. Logs to the path in $4.
set -u
PID="$1"
TARGET="$2"
STAGED="$3"
LOG="$4"
WORK="$5"

exec >>"$LOG" 2>&1
say() { printf '%s %s\n' "$(/bin/date -u '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || /bin/date)" "$*"; }

say "update helper started for pid $PID"
case "$TARGET" in
  *.app) ;;
  *) say "refusing: target is not an app bundle"; exit 1 ;;
esac
case "$STAGED" in
  *.app) ;;
  *) say "refusing: staged path is not an app bundle"; exit 1 ;;
esac

while /bin/kill -0 "$PID" 2>/dev/null; do
  /bin/sleep 0.25
done
/bin/sleep 0.4

BACKUP="$TARGET.shortlist-old-$$"
/bin/rm -rf "$BACKUP"
if ! /bin/mv "$TARGET" "$BACKUP"; then
  say "could not move the old app out of the way; leaving it untouched"
  /usr/bin/open "$TARGET" 2>/dev/null || true
  exit 1
fi

if /usr/bin/ditto "$STAGED" "$TARGET"; then
  /bin/chmod +x "$TARGET/Contents/MacOS/shortlist" 2>/dev/null || true
  /usr/bin/xattr -dr com.apple.quarantine "$TARGET" 2>/dev/null || true
  if /usr/bin/open "$TARGET" 2>/dev/null; then
    /bin/rm -rf "$BACKUP"
    say "installed; relaunched"
    /bin/rm -rf "$WORK"
    exit 0
  fi
  say "could not relaunch the new version; restoring the previous version"
  /bin/rm -rf "$TARGET"
  if /bin/mv "$BACKUP" "$TARGET"; then
    /usr/bin/open "$TARGET" 2>/dev/null || true
  else
    say "restore failed; the previous app is at $BACKUP"
  fi
  exit 1
fi

say "copy failed; restoring the previous version"
/bin/rm -rf "$TARGET"
if /bin/mv "$BACKUP" "$TARGET"; then
  /usr/bin/open "$TARGET" 2>/dev/null || true
else
  say "restore failed; the previous app is at $BACKUP"
fi
exit 1
"#;

/// Linux/portable-folder helper. Kept POSIX-sh so it also works when a macOS
/// build is run from a folder instead of an `.app` bundle.
#[cfg_attr(windows, allow(dead_code))]
const UNIX_HELPER_SCRIPT: &str = r#"#!/bin/sh
# Shortlist auto-update helper for portable Linux/macOS-folder installs.
set -u
PID="$1"
TARGET="$2"
STAGED="$3"
LOG="$4"
WORK="$5"

exec >>"$LOG" 2>&1
say() { printf '%s %s\n' "$(/bin/date -u '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || /bin/date)" "$*"; }

launch_app() {
  root="$1"
  cd "$root" || return 1
  if command -v nohup >/dev/null 2>&1; then
    nohup ./shortlist </dev/null >/dev/null 2>&1 &
  else
    ./shortlist </dev/null >/dev/null 2>&1 &
  fi
  LAUNCHED_PID=$!
  return 0
}

say "update helper started for pid $PID"
case "$TARGET" in
  "/"|"") say "refusing: unsafe target path"; exit 1 ;;
esac
case "$STAGED" in
  "/"|"") say "refusing: unsafe staged path"; exit 1 ;;
esac

while /bin/kill -0 "$PID" 2>/dev/null; do
  /bin/sleep 0.25
done
/bin/sleep 0.4

BACKUP="$TARGET.shortlist-old-$$"
/bin/rm -rf "$BACKUP"
if ! /bin/mv "$TARGET" "$BACKUP"; then
  say "could not move the old app out of the way; leaving it untouched"
  launch_app "$TARGET" >/dev/null
  exit 1
fi

if /bin/cp -a "$STAGED" "$TARGET"; then
  /bin/chmod +x "$TARGET/shortlist" "$TARGET/shortlist-pdf" 2>/dev/null || true
  LAUNCHED_PID=""
  launch_app "$TARGET" || true
  /bin/sleep 2
  if [ -n "$LAUNCHED_PID" ] && /bin/kill -0 "$LAUNCHED_PID" 2>/dev/null; then
    /bin/rm -rf "$BACKUP"
    say "installed; relaunched"
    /bin/rm -rf "$WORK"
    exit 0
  fi
  say "could not relaunch the new version; restoring the previous version"
  /bin/rm -rf "$TARGET"
  if /bin/mv "$BACKUP" "$TARGET"; then
    launch_app "$TARGET" >/dev/null
  else
    say "restore failed; the previous app is at $BACKUP"
  fi
  exit 1
fi

say "copy failed; restoring the previous version"
/bin/rm -rf "$TARGET"
if /bin/mv "$BACKUP" "$TARGET"; then
  launch_app "$TARGET" >/dev/null
else
  say "restore failed; the previous app is at $BACKUP"
fi
exit 1
"#;

/// Windows update helper. PowerShell 5.1 is present on Windows 10/11. It waits
/// for the app to exit, swaps the portable folder with robocopy, and relaunches.
#[cfg_attr(not(windows), allow(dead_code))]
const WINDOWS_HELPER_SCRIPT: &str = r#"param(
    [Parameter(Mandatory = $true)][int]$AppPid,
    [Parameter(Mandatory = $true)][string]$Target,
    [Parameter(Mandatory = $true)][string]$Staged,
    [Parameter(Mandatory = $true)][string]$Log,
    [Parameter(Mandatory = $true)][string]$Work
)
$ErrorActionPreference = "Stop"

function Write-Log([string]$Message) {
    $stamp = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    Add-Content -LiteralPath $Log -Value "$stamp $Message"
}

function Remove-Path([string]$PathToRemove) {
    if (Test-Path -LiteralPath $PathToRemove) {
        Remove-Item -LiteralPath $PathToRemove -Recurse -Force
    }
}

function Start-Shortlist([string]$Root) {
    $exe = Join-Path $Root "shortlist.exe"
    $process = Start-Process -FilePath $exe -WorkingDirectory $Root -PassThru
    Start-Sleep -Seconds 2
    if ($process.HasExited) {
        throw "Shortlist exited immediately after launch"
    }
    return $process
}

try {
    Write-Log "update helper started for pid $AppPid"
    if ([string]::IsNullOrWhiteSpace($Target) -or $Target -match '^[A-Za-z]:\\?$') {
        throw "unsafe target path"
    }
    if ([string]::IsNullOrWhiteSpace($Staged) -or $Staged -match '^[A-Za-z]:\\?$') {
        throw "unsafe staged path"
    }
    while (Get-Process -Id $AppPid -ErrorAction SilentlyContinue) {
        Start-Sleep -Milliseconds 250
    }
    Start-Sleep -Milliseconds 400

    $backup = "$Target.shortlist-old-$AppPid"
    Remove-Path $backup

    $moved = $false
    for ($attempt = 0; $attempt -lt 20; $attempt++) {
        try {
            Move-Item -LiteralPath $Target -Destination $backup -Force -ErrorAction Stop
            $moved = $true
            break
        } catch {
            Start-Sleep -Milliseconds 250
        }
    }
    if (-not $moved) {
        throw "could not move the old app out of the way"
    }

    try {
        New-Item -ItemType Directory -Path $Target -Force | Out-Null
        robocopy $Staged $Target /E /COPY:DAT /R:5 /W:1 /NFL /NDL /NJH /NJS /NP | Out-Null
        if ($LASTEXITCODE -ge 8) {
            throw "robocopy failed with code $LASTEXITCODE"
        }
        Start-Shortlist $Target | Out-Null
        try { Remove-Path $backup } catch { Write-Log "backup cleanup failed: $_" }
        Write-Log "installed; relaunched"
        try { Remove-Path $Work } catch { Write-Log "work cleanup failed: $_" }
        exit 0
    } catch {
        Write-Log "copy or relaunch failed: $_; restoring the previous version"
        Remove-Path $Target
        if (Test-Path -LiteralPath $backup) {
            Move-Item -LiteralPath $backup -Destination $Target -Force
        }
        try {
            Start-Shortlist $Target | Out-Null
        } catch {
            Write-Log "restore relaunch failed: $_"
        }
        exit 1
    }
} catch {
    Write-Log "update failed: $_"
    try {
        if (Test-Path -LiteralPath $Target) {
            Start-Shortlist $Target | Out-Null
        }
    } catch {
        Write-Log "existing app relaunch failed: $_"
    }
    exit 1
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, asset_names: &[&str]) -> GithubRelease {
        GithubRelease {
            tag_name: tag.to_string(),
            name: Some(format!("Shortlist {tag}")),
            body: Some("Fixes and improvements.".into()),
            html_url: format!("https://github.com/{DEFAULT_REPO}/releases/tag/{tag}"),
            published_at: Some("2026-01-01T00:00:00Z".into()),
            draft: false,
            prerelease: false,
            assets: asset_names
                .iter()
                .map(|name| GithubAsset {
                    name: name.to_string(),
                    browser_download_url: format!(
                        "https://github.com/{DEFAULT_REPO}/releases/download/{tag}/{name}"
                    ),
                    size: 1024,
                })
                .collect(),
        }
    }

    #[test]
    fn older_versions_are_up_to_date() {
        let current = Version::parse("0.2.0").unwrap();
        let result = release_from_github(
            release("v0.1.9", &["Shortlist-macos-aarch64.zip", CHECKSUMS_ASSET]),
            &current,
            "macos",
            "aarch64",
        )
        .unwrap();

        assert!(
            matches!(result, CheckResult::UpToDate { latest, .. } if latest.to_string() == "0.1.9")
        );
    }

    #[test]
    fn newer_versions_select_the_matching_asset() {
        let current = Version::parse("0.1.0").unwrap();
        let result = release_from_github(
            release(
                "v0.2.0",
                &[
                    "Shortlist-macos-x86_64.zip",
                    "Shortlist-macos-aarch64.zip",
                    CHECKSUMS_ASSET,
                ],
            ),
            &current,
            "macos",
            "aarch64",
        )
        .unwrap();

        let CheckResult::Available(info) = result else {
            panic!("expected an available update");
        };
        assert_eq!(info.version.to_string(), "0.2.0");
        assert_eq!(
            info.asset.as_ref().map(|asset| asset.name.as_str()),
            Some("Shortlist-macos-aarch64.zip")
        );
        assert!(info.checksums_url.is_some());
    }

    #[test]
    fn unsupported_platforms_still_see_release_notes() {
        let current = Version::parse("0.1.0").unwrap();
        let result = release_from_github(
            release("v0.2.0", &["Shortlist-macos-aarch64.zip", CHECKSUMS_ASSET]),
            &current,
            "linux",
            "x86_64",
        )
        .unwrap();

        let CheckResult::Available(info) = result else {
            panic!("expected an available update");
        };
        assert!(info.asset.is_none());
    }

    #[test]
    fn tags_must_be_semver() {
        assert_eq!(parse_tag_version("v1.2.3").unwrap().to_string(), "1.2.3");
        assert!(parse_tag_version("nightly").is_err());
    }

    #[test]
    fn asset_names_are_platform_specific() {
        assert_eq!(
            expected_asset_name("macos", "aarch64").as_deref(),
            Some("Shortlist-macos-aarch64.zip")
        );
        assert_eq!(
            expected_asset_name("macos", "x86_64").as_deref(),
            Some("Shortlist-macos-x86_64.zip")
        );
        assert_eq!(
            expected_asset_name("windows", "x86_64").as_deref(),
            Some("Shortlist-windows-x86_64.zip")
        );
        assert_eq!(
            expected_asset_name("linux", "x86_64").as_deref(),
            Some("Shortlist-linux-x86_64.zip")
        );
        assert_eq!(
            expected_asset_name("linux", "aarch64").as_deref(),
            Some("Shortlist-linux-aarch64.zip")
        );
    }

    #[test]
    fn portable_install_markers_are_validated() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join(APP_DIR_NAME);
        fs::create_dir_all(&app).unwrap();
        fs::write(app.join(platform_executable()), b"binary").unwrap();
        fs::write(app.join(platform_pdf_helper()), b"helper").unwrap();

        let version = Version::parse("0.2.0").unwrap();
        let manifest = InstallManifest {
            app: APP_MARKER_ID.to_string(),
            version: version.to_string(),
            platform: std::env::consts::OS.to_string(),
            executable: platform_executable().to_string(),
        };
        fs::write(
            app.join(INSTALL_MARKER),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        validate_directory_install(&app, &version).unwrap();
        assert!(validate_directory_install(&app, &Version::parse("0.3.0").unwrap()).is_err());

        let mut wrong = manifest.clone();
        wrong.platform = "other".to_string();
        fs::write(
            app.join(INSTALL_MARKER),
            serde_json::to_vec(&wrong).unwrap(),
        )
        .unwrap();
        assert!(validate_directory_install(&app, &version).is_err());

        let installation = directory_installation(&app.join(platform_executable()), "linux");
        assert!(installation.is_none());
    }

    #[test]
    fn portable_install_detects_the_running_folder() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join(APP_DIR_NAME);
        fs::create_dir_all(&app).unwrap();
        let exe = app.join(platform_executable());
        fs::write(&exe, b"binary").unwrap();
        let manifest = InstallManifest {
            app: APP_MARKER_ID.to_string(),
            version: "0.2.0".to_string(),
            platform: "linux".to_string(),
            executable: platform_executable().to_string(),
        };
        fs::write(
            app.join(INSTALL_MARKER),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        let installation = directory_installation(&exe, "linux").expect("marker should be found");
        assert_eq!(installation.kind, InstallKind::Directory);
        assert_eq!(installation.root, app);
        assert!(directory_installation(&exe, "windows").is_none());
    }

    #[test]
    fn checksum_manifest_accepts_common_formats() {
        let hash = "a".repeat(64);
        let text = format!(
            "{hash}  Shortlist-macos-aarch64.zip\n{} *Shortlist-macos-x86_64.zip\n# comment\nshort bad\n",
            "b".repeat(64)
        );
        let checksums = parse_checksums(&text);
        assert_eq!(checksums.get("Shortlist-macos-aarch64.zip"), Some(&hash));
        assert_eq!(
            checksums.get("Shortlist-macos-x86_64.zip"),
            Some(&"b".repeat(64))
        );
        assert_eq!(checksums.len(), 2);
    }

    #[test]
    fn extraction_finds_and_validates_a_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("update.zip");
        let file = File::create(&archive_path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        let file_options = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
        let directory_options = zip::write::SimpleFileOptions::default().unix_permissions(0o755);

        archive
            .add_directory("Shortlist.app/Contents/MacOS/", directory_options)
            .unwrap();
        archive
            .start_file("Shortlist.app/Contents/MacOS/shortlist", file_options)
            .unwrap();
        archive.write_all(b"#!/bin/sh\n").unwrap();
        archive
            .start_file(
                "Shortlist.app/Contents/Info.plist",
                zip::write::SimpleFileOptions::default().unix_permissions(0o644),
            )
            .unwrap();
        let version = Version::parse("0.2.0").unwrap();
        archive
            .write_all(
                format!(
                    "<key>CFBundleIdentifier</key><string>{APP_BUNDLE_ID}</string>\
                     <key>CFBundleShortVersionString</key><string>{version}</string>"
                )
                .as_bytes(),
            )
            .unwrap();
        archive.finish().unwrap();

        let extract_dir = dir.path().join("extracted");
        fs::create_dir_all(&extract_dir).unwrap();
        extract_zip(&archive_path, &extract_dir).unwrap();
        let bundle = find_mac_bundle(&extract_dir).unwrap();
        validate_mac_bundle(&bundle, &version).unwrap();
        assert_eq!(
            bundle.file_name().and_then(|name| name.to_str()),
            Some("Shortlist.app")
        );
    }

    #[test]
    #[ignore = "set SHORTLIST_TEST_ARCHIVE to a packaged release zip"]
    fn real_release_archive_stages() {
        let archive = std::env::var("SHORTLIST_TEST_ARCHIVE")
            .expect("SHORTLIST_TEST_ARCHIVE must point at a release zip");
        let dir = tempfile::tempdir().unwrap();
        let extract_dir = dir.path().join("extracted");
        fs::create_dir_all(&extract_dir).unwrap();
        extract_zip(Path::new(&archive), &extract_dir).unwrap();
        let bundle = find_mac_bundle(&extract_dir).unwrap();
        validate_mac_bundle(&bundle, &current_version()).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(bundle.join("Contents/MacOS/shortlist"))
                .unwrap()
                .permissions()
                .mode();
            assert!(mode & 0o111 != 0, "the extracted app must be executable");
        }
    }

    #[test]
    fn extraction_rejects_parent_directory_entries() {
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("unsafe.zip");
        let file = File::create(&archive_path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file(
                "../escape.txt",
                zip::write::SimpleFileOptions::default().unix_permissions(0o644),
            )
            .unwrap();
        archive.write_all(b"nope").unwrap();
        archive.finish().unwrap();

        let extract_dir = dir.path().join("extracted");
        fs::create_dir_all(&extract_dir).unwrap();
        assert!(extract_zip(&archive_path, &extract_dir).is_err());
        assert!(!dir.path().join("escape.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn helper_scripts_are_valid_shell() {
        let dir = tempfile::tempdir().unwrap();
        for (name, script) in [
            ("install-macos.sh", MAC_HELPER_SCRIPT),
            ("install-unix.sh", UNIX_HELPER_SCRIPT),
        ] {
            let path = dir.path().join(name);
            fs::write(&path, script).unwrap();
            let status = Command::new("/bin/sh")
                .arg("-n")
                .arg(&path)
                .status()
                .unwrap();
            assert!(status.success(), "{name} failed its shell syntax check");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_helper_parses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install.ps1");
        fs::write(&path, WINDOWS_HELPER_SCRIPT).unwrap();
        let script = format!(
            "[void][scriptblock]::Create((Get-Content -LiteralPath '{}' -Raw))",
            path.display()
        );
        let status = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status()
            .unwrap();
        assert!(status.success(), "the Windows helper has invalid syntax");
    }

    #[test]
    fn download_urls_must_point_at_this_repo() {
        assert!(
            validate_download_url(&format!(
                "https://github.com/{}/releases/download/v0.2.0/Shortlist-macos-aarch64.zip",
                repo()
            ))
            .is_ok()
        );
        assert!(validate_download_url("https://example.com/Shortlist-macos-aarch64.zip").is_err());
    }
}
