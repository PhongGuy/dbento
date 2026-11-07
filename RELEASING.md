# Releasing Dbento

## Overview

A release is a `v<version>` tag. Pushing it runs `.github/workflows/release.yml`, which checks that the tag matches the workspace version, builds the five targets (Linux x86_64 and arm64, Windows x64, macOS Apple silicon and Intel), signs the Windows files in a separate job, adds a copy of every package and archive under a name without the version or the `-unsigned` marker (such as `Dbento_aarch64.dmg`, which the README links through `/releases/latest/download/`), writes one `SHA256SUMS` over every file and drafts a GitHub release. The draft's notes come from `packaging/release-notes.md`, with the version filled in and a What's Changed section taken from GitHub's generated notes since the previous tag, which group the merged pull requests by label through `.github/release.yml`; `scripts/release-notes.sh v<version>` prints the same notes locally with an authenticated `gh`. A human reviews the draft and publishes it. Nothing is published automatically.

The macOS and Windows legs of the workflow have not yet run on real runners. Their artifact names, signing steps and runner tools are taken from the scripts and are unconfirmed until the first rehearsal below has passed.

## Who acts

Pushing a tag, publishing the draft, uploading anything and dispatching any workflow run are the maintainer's own actions, each taken only with their explicit approval.

## Signing

The build jobs compile, which runs every dependency's build script, so they get no secret and restore no cache. Windows files leave the build job marked `-unsigned`. The `sign` job is the only job bound to the `release` environment and the only one that sees the certificate: it runs on a Windows runner on a tag only, downloads the Windows artifact, signs it with the runner's `signtool`, and replaces the artifact. It compiles and installs nothing. The `checksums` job, which drafts the release, holds `contents: write` but no signing secret.

The `release` environment accepts only `v*` tags and waits for the maintainer's approval, so a tag run pauses at `sign` until it is approved.

### macOS

There is no Apple Developer account and no notarization. The `.app` inside the `.dmg` and the `dbento` binary in the CLI archive are ad-hoc signed.

Gatekeeper blocks the first open of an app that is not notarized. Move `Dbento.app` to `/Applications` before the first launch, then clear the quarantine flag:

```sh
xattr -dr com.apple.quarantine /Applications/Dbento.app
```

For the CLI, run `xattr -d com.apple.quarantine dbento`.

### Windows

Windows artifacts are signed with a self-signed code-signing certificate, created once on the maintainer's machine. With OpenSSL:

```sh
openssl req -x509 -newkey rsa:3072 -sha256 -days 1825 -noenc -subj "/CN=Dbento" -addext "keyUsage=critical,digitalSignature" -addext "extendedKeyUsage=codeSigning" -keyout dbento-code.key -out dbento-code.crt
openssl pkcs12 -export -inkey dbento-code.key -in dbento-code.crt -name Dbento -out dbento-code.pfx
base64 -w0 dbento-code.pfx | gh secret set WINDOWS_CERTIFICATE --env release
gh secret set WINDOWS_CERTIFICATE_PASSWORD --env release
```

The last command prompts for the export password. On Windows, `New-SelfSignedCertificate -Type CodeSigningCert` plus `Export-PfxCertificate` does the same. Keep the key and the `.pfx` offline.

Both are secrets of the `release` environment, never repo secrets. Set both or neither. With both, the installer and the `dbento.exe` inside the CLI `.zip` are signed; the `dbento-app.exe` the installer puts on disk and its uninstaller are not, since signing them would mean packaging beside the key. With neither, the installer and the CLI `.zip` keep `-unsigned` in their names, such as `dbento-app_<version>_x64-setup-unsigned.exe`. With only one of them set, the `sign` job fails. SmartScreen warns either way, because the certificate chains to no trusted root.

### Linux

Linux has no OS signing. Verify downloads with:

```sh
sha256sum -c SHA256SUMS
```

## Rehearsal

Dispatch a dry run from `master`. It builds the chosen platforms, signs nothing (the `sign` job runs only on a tag) and drafts nothing, so its Windows files keep `-unsigned`:

```sh
gh workflow run release.yml --ref master -f dry_run=true -f platforms=linux,windows,macos-arm64
gh run watch <id> --exit-status
```

The `release-<version>` artifact of the run holds every file plus `SHA256SUMS`. Download it with `gh run download <id> -n release-<version>` and run `sha256sum -c SHA256SUMS` there.

The platform tokens are `linux`, `windows`, `macos-arm64` and `macos-intel`. The default leaves out `macos-intel`, because every macOS minute bills 10x; add it when the Intel leg needs checking. A real release must build all four tokens, and the guard refuses a subset unless `dry_run` is set.

## Tagging

1. Set `[workspace.package] version` in `Cargo.toml` and commit.
2. `git tag v<version>`
3. `git push origin v<version>`

The guard fails on a tag that does not match the workspace version, before any build starts. A branch can only start a dry run. A run dispatched on a tag ref with `dry_run=false` drafts a release too, exactly as pushing the tag does. One dispatched on a tag ref with `dry_run=true` signs (after approval) but drafts nothing, which is how to rehearse signing.

## Draft review

1. Download the draft's files and run `sha256sum -c SHA256SUMS`.
2. Mount the `.dmg` and run `codesign -v` on the `.app`.
3. Check the Windows installer's signature, or its `-unsigned` name.
4. Install the `.deb` on stock Ubuntu 24.04; it installs `/usr/bin/dbento` and `/usr/bin/dbento-app`.
5. Check that each package and CLI archive holds `THIRD-PARTY-NOTICES.html`, and that the release carries no loose copy.
6. Edit the notes: replace the intro placeholder with one to three sentences on this release's headline changes, and reword What's Changed where it helps.
7. Check the placeholder is gone; this must print `0`:

   ```sh
   gh release view v<version> --json body -q .body | grep -c '<!-- intro:'
   ```

8. Publish the release in the GitHub UI.

## First launch

On macOS, move `Dbento.app` to `/Applications` before the first launch, then run `xattr -dr com.apple.quarantine /Applications/Dbento.app`. Launching from the download folder runs the app from a temporary translocated path, and the MCP connect command the app shows would name that path; from `/Applications` it names a stable one.

The AppImage cannot put its inner `dbento` on `PATH`, so AppImage users also take the CLI archive for their architecture to get `dbento`. The `.deb` installs `dbento` next to the app. The `.dmg` and the Windows installer are built to bundle it too; the first rehearsal is what confirms it.

## Actions minutes

Each job is billed in whole minutes; Windows bills 2x and macOS 10x. No release run has been measured yet, so every figure below is ASSUMED until the first rehearsal records real ones:

- ASSUMED: a CI run on `master` is about 20 to 30 billed minutes.
- ASSUMED: a full release is about 560: Linux about 60, Windows 30 x 2, macOS 2 x 22 x 10.
- ASSUMED: the default `linux,windows,macos-arm64` rehearsal is about 340.

Every job's `timeout-minutes` caps what a hung run can cost: `guard` 5, `build` 90, `sign` 15, `checksums` 45, and the CI `gate` 60. Release jobs restore no cache, so every build compiles from scratch.
