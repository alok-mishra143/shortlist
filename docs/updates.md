# Updates

Shortlist updates itself from GitHub Releases. The app checks the newest
published release, downloads the build for the current machine, verifies it,
swaps the running app, and reopens. Users do not reinstall anything.

The check is optional and quiet: it runs once, a couple of seconds after launch,
and can be turned off under **Settings → Updates**. Only the update check talks
to GitHub. Candidate data, the SQLite database, embeddings, and notes never
leave the machine and are stored outside the app directory, so they survive an
update.

macOS, Windows, and Linux all run the same updater. macOS replaces a
`Shortlist.app`; Windows and Linux replace a portable `Shortlist/` folder.

## First install

Download the archive for your system from **GitHub → Releases**:

| System | Asset | After extracting |
|:--|:--|:--|
| Apple Silicon | `Shortlist-macos-aarch64.zip` | Move `Shortlist.app` to Applications |
| Intel Mac | `Shortlist-macos-x86_64.zip` | Move `Shortlist.app` to Applications |
| Windows x64 | `Shortlist-windows-x86_64.zip` | Extract the `Shortlist` folder and run `shortlist.exe` |
| Linux x64 | `Shortlist-linux-x86_64.zip` | Extract the `Shortlist` folder and run `./shortlist` |

Keep the whole `Shortlist` folder together on Windows and Linux. The
`shortlist-install.json` marker inside it is what lets the app update itself in
place; moving the executable out on its own disables automatic installation.

Release builds are not notarized yet. The first macOS launch may need
right-click → **Open**, Windows may show a SmartScreen prompt, and Linux may
need `chmod +x shortlist shortlist-pdf` if the archive was unpacked with a tool
that drops permissions. The updater handles quarantine and executable bits on
the new build.

Linux needs the usual GPUI desktop runtime: X11 or Wayland, `libxkbcommon`,
a Vulkan driver, and OpenSSL 3. These are standard on current desktop
distributions. The CI build runs on Ubuntu 24.04 because the prebuilt ONNX
Runtime that `ort` links against needs glibc 2.38+, so the released binary
targets Ubuntu 24.04+ / Debian 13+.

## What a user sees

1. Shortlist opens and, if automatic checks are on, asks GitHub for the latest
   release.
2. When a newer version exists, a banner appears above the current screen and
   the **Settings → Updates** card shows the version and release notes.
3. **Update & restart** downloads the release archive, verifies its SHA-256,
   replaces the running app, and relaunches Shortlist.
4. If anything fails, the app stays on the current version and shows the error.
   The platform helper restores the previous app if copying or relaunching
   fails.

The banner can be dismissed with **Later**; the update stays available in
Settings. Downloads can be cancelled.

## Cutting a release

The updater installs built artifacts, not source commits. A release therefore
goes through GitHub Actions.

### Automatic, on every push to `main`

`.github/workflows/auto-release.yml`:

1. bumps the workspace patch version in `Cargo.toml` and refreshes `Cargo.lock`,
2. commits `Release vX.Y.Z`, creates an annotated tag, and pushes both,
3. calls `.github/workflows/release.yml` to build macOS (Apple Silicon and
   Intel), Windows x64, and Linux x64 from that tag, and
4. publishes a GitHub Release with every zip and one `checksums.txt`.

Installed apps see the new release on their next check. The first release is
normally `v0.1.1` even though the working tree started at `0.1.0`; that is what
makes existing installs see an update.

Pushing to `main` needs the workflow token to push a commit and tag. If `main`
gets branch protection, allow `github-actions[bot]` to bypass it.

### By hand, for minor or major bumps

```bash
./scripts/release.sh minor --push     # or major / patch
```

`release.sh` bumps the version it is told to, commits `Release vX.Y.Z`, and
pushes the commit and tag. The tag triggers the same release workflow.
Auto-release skips commits whose message starts with `Release v`, so the two
paths never publish the same version twice.

If Actions is unavailable, build on each target with the packaging scripts below
and upload the zips plus a combined `checksums.txt` to the GitHub release
yourself. The app only cares about the asset names and the checksum lines.

### Asset contract

The asset names are part of the update protocol and must not change without
changing `crates/update/src/lib.rs`:

| Asset | Purpose |
|:--|:--|
| `Shortlist-macos-aarch64.zip` | Apple Silicon build |
| `Shortlist-macos-x86_64.zip` | Intel Mac build |
| `Shortlist-windows-x86_64.zip` | Windows x64 portable build |
| `Shortlist-linux-x86_64.zip` | Linux x64 portable build |
| `checksums.txt` | SHA-256 lines for every zip |

The ARM names (`Shortlist-windows-aarch64.zip`, `Shortlist-linux-aarch64.zip`)
are understood by the updater, but the standard release workflow currently
builds the four assets above.

**macOS layout**

```
Shortlist.app/
  Contents/
    MacOS/shortlist
    MacOS/shortlist-pdf
    Resources/models/e5-small-v2/
    Info.plist
```

**Windows / Linux layout**

```
Shortlist/
  shortlist(.exe)
  shortlist-pdf(.exe)
  shortlist-install.json
  assets/models/e5-small-v2/
```

The model makes a packaged install work offline out of the box; it also makes
each update roughly 45 MB.

Build the packages locally with:

```bash
./scripts/package-macos.sh
./scripts/package-linux.sh
pwsh -File scripts/package-windows.ps1

# exercise extraction + bundle validation against the real macOS archive
SHORTLIST_TEST_ARCHIVE="$PWD/dist/Shortlist-macos-aarch64.zip" \
  cargo test -p shortlist-update real_release_archive_stages -- --ignored
```

Linux build dependencies (Ubuntu/Debian) are installed by
`scripts/install-linux-deps.sh`, which CI also uses.

## How the updater protects itself

- **HTTPS only.** Download URLs must start with
  `https://github.com/alok-mishra143/shortlist/releases/download/`.
- **Checksums are mandatory.** The archive is installed only when its SHA-256
  matches the `checksums.txt` attached to the same release. Release pages
  without checksums are viewable but never auto-installed.
- **Payload identity check.** A macOS update must contain
  `com.shortlist.app` and `Contents/MacOS/shortlist`; a Windows/Linux update
  must contain a matching `shortlist-install.json` marker, version, platform,
  executable, and PDF helper.
- **Path traversal and bomb limits.** Archive entries are confined to the
  staging directory, symlinks are rejected, and extracted size/count is capped.
- **No privilege escalation.** The updater never asks for `sudo` or an
  administrator prompt. If the app's folder is not writable, it explains the
  problem and leaves the app untouched.
- **Rollback.** The helper moves the old app aside before copying. If copying or
  relaunching fails, it restores the old app and opens it again.

Helpers are dependency-free:

- macOS and Linux run a small `/bin/sh` script.
- Windows runs a generated PowerShell 5.1 script with `robocopy` and retries.

The helper's log lives at `<data>/logs/update.log`:

| System | Default log |
|:--|:--|
| macOS | `~/Library/Application Support/Shortlist/logs/update.log` |
| Windows | `%APPDATA%\Shortlist\logs\update.log` |
| Linux | `~/.local/share/Shortlist/logs/update.log` |

(`SHORTLIST_DATA_DIR` moves the data root and therefore the log.)

## Development builds

`./scripts/run.sh` and `cargo run` launch from `target/`, not from a packaged
install. Automatic installation is disabled there: **Download update** opens the
GitHub release instead. Version checks still work, which makes it easy to test
the banner and settings card.

## Signing and notarization

CI ad-hoc signs the macOS app. That is enough for the remove-and-replace flow
because the helper clears the quarantine attribute, but first installs
distributed outside GitHub should be signed with a Developer ID certificate and
notarized. Windows portable builds are currently unsigned and may trigger
SmartScreen. The update path itself does not depend on code signing; it trusts
the release checksum over HTTPS.

## Forks

Build with a different repository if needed:

```bash
SHORTLIST_UPDATE_REPO=owner/repo cargo build --release -p shortlist-app
```

The release workflow sets this automatically from `github.repository` for every
platform job, so a fork's builds update from the fork. Asset names and the app
bundle identifier stay the same.
