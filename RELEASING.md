# Releasing Dbento

## Overview

A release is a `v<version>` tag. Pushing it runs `.github/workflows/release.yml`, which checks that the tag matches the workspace version, builds the five targets (Linux x86_64 and arm64, Windows x64, macOS Apple silicon and Intel), adds a copy of every package and archive under a name without the version (such as `Dbento_aarch64.dmg`, which the README links through `/releases/latest/download/`), writes one `SHA256SUMS` over every package and archive, writes the update manifest `latest.json`, signs it, attests every file and drafts a GitHub release. The draft's notes come from `packaging/release-notes.md`, with the version filled in and a What's Changed section taken from GitHub's generated notes since the previous tag, which group the merged pull requests by label through `.github/release.yml`; `scripts/release-notes.sh v<version>` prints the same notes locally with an authenticated `gh`. A human reviews the draft and publishes it. Nothing is published automatically.

The macOS and Windows legs of the workflow have not yet run on real runners. Their artifact names, ad-hoc signing and runner tools are taken from the scripts and are unconfirmed until the first rehearsal below has passed.

## Who acts

Pushing a tag, publishing the draft, uploading anything and dispatching any workflow run are the maintainer's own actions, each taken only with their explicit approval.

## Signing

The build jobs compile, which runs every dependency's build script, so they get no secret and restore no cache.

Provenance comes from a GitHub build attestation. `checksums` adds the stable copies and `SHA256SUMS`, and on a run that drafts it writes `update/latest.json` with `scripts/release.sh manifest`. It uploads the finished set, `dist/` and `update/`, as the `release-<version>` artifact. The `attest` job is the only job bound to the `release` environment, which accepts only `v*` tags and waits for the maintainer's approval, so a tag run pauses there, and only it holds `id-token: write`.

The `attest` job compiles nothing; it runs only the distro `minisign`, `base64` and shell builtins. It installs `minisign` from Ubuntu's signed archive at a pinned version before any step holds a secret. The signing key `CARGO_PACKAGER_SIGN_PRIVATE_KEY` and its password `CARGO_PACKAGER_SIGN_PRIVATE_KEY_PASSWORD` are set only on the signing step's environment; that step pipes both to `minisign`, so neither touches disk or a process's arguments, and writes the signature as `update/latest.json.sig`. The job then attests every file, `SHA256SUMS`, `latest.json` and `latest.json.sig` included, with `actions/attest`. `draft` uploads the signed set, so every file in the release is attested. Attesting `SHA256SUMS` too means a user who verifies only the sums file can trust every checksum in it.

The `.app.tar.gz` files and the raw CLI binaries are update-only: the updater downloads them, people do not. They live in `update/`, kept out of `dist/` and `SHA256SUMS`, so their digests are in the signed manifest and the attestation instead. The AppImages and the Windows installer serve both purposes and ship from `dist/`.

Anyone can check a download:

```sh
gh attestation verify <file> --repo PhongGuy/dbento
```

A published release's assets are never replaced; a bad release is fixed by a new version.

### macOS

There is no Apple Developer account and no notarization. The `.app` inside the `.dmg` and the `dbento` binary in the CLI archive are ad-hoc signed.

Gatekeeper blocks the first open of an app that is not notarized. Move `Dbento.app` to `/Applications` before the first launch, then clear the quarantine flag:

```sh
xattr -dr com.apple.quarantine /Applications/Dbento.app
```

For the CLI, run `xattr -d com.apple.quarantine dbento`.

### Windows

Windows files are not Authenticode-signed: that needs a CA-issued certificate, and a self-signed one only moves the SmartScreen warning. SmartScreen warns about an unknown publisher on the installer.

### Linux

Linux has no OS signing. Verify downloads with:

```sh
sha256sum -c SHA256SUMS
```

## Keys

The app trusts the public keys in `assets/update/keys.txt`, one base64 line each, compiled in. The current key ID is `40786E97EA264BC5`, and the `release` environment already holds its private key. A build only accepts a manifest signed by a key it was shipped with.

A new pair is generated once on the maintainer's machine. The private key lives online only as a secret of the `release` environment, and the key file itself stays offline as the backup, since the secret is its only online copy:

```sh
cargo packager signer generate --path dbento-update.key
gh secret set CARGO_PACKAGER_SIGN_PRIVATE_KEY --env release < dbento-update.key
gh secret set CARGO_PACKAGER_SIGN_PRIVATE_KEY_PASSWORD --env release
```

Never set them as repository secrets, which reach any workflow on any branch.

Append the public key, the `.pub` file's content as written, to `assets/update/keys.txt` and commit it.

### Rotation and compromise

Rotation takes three releases. First, generate a new pair and add its public key to `keys.txt` beside the old one. Second, ship that release signed by the old key, so every client learns the new key from a manifest it can verify; then switch the release secrets to the new key. Third, remove the old line in a later release, once the new key has signed one.

If a private key leaks, every client that still lists it would accept a manifest an attacker signs with it. Delete the leaked secrets, then ship a release that removes its line from `keys.txt` and is signed by another key those clients already trust. Clients that trust only the leaked key cannot verify a release signed by the new key, so tell their users to reinstall from the Releases page.

## Rehearsal

Dispatch a dry run from `master`. It builds the chosen platforms and drafts nothing:

```sh
gh workflow run release.yml --ref master -f dry_run=true -f platforms=linux,windows,macos-arm64
gh run watch <id> --exit-status
```

The `release-<version>` artifact of the run holds every file plus `SHA256SUMS`. Download it with `gh run download <id> -n release-<version>` and run `sha256sum -c SHA256SUMS` there.

The platform tokens are `linux`, `windows`, `macos-arm64` and `macos-intel`. The default leaves out `macos-intel`, because every macOS minute bills 10x; add it when the Intel leg needs checking. A real release must build all four tokens, and the guard refuses a subset unless `dry_run` is set.

## Tagging

1. Set `[workspace.package] version` in `Cargo.toml` and commit.
2. Once that commit is on `master`, dispatch CI with its Windows run and wait for it to pass. Only that run compiles and tests the updater's Windows code (the NSIS hand-off and the CLI's self-replace); push and pull request runs skip it.

   ```sh
   gh workflow run ci.yml --ref master -f windows=true
   gh run watch <id> --exit-status
   ```

3. `git tag v<version>`
4. `git push origin v<version>`

The guard fails on a tag that does not match the workspace version, before any build starts. A branch can only start a dry run. A run dispatched on a tag ref with `dry_run=false` drafts a release too, exactly as pushing the tag does. One dispatched on a tag ref with `dry_run=true` attests (after approval) but drafts nothing.

## Draft review

1. Download the draft's files, run `sha256sum -c SHA256SUMS`, and run `gh attestation verify` on one of them.
2. Check the update manifest: run `gh attestation verify latest.json --repo PhongGuy/dbento`, then verify its signature against the committed key, and check that its version and file names match the release:

   ```sh
   base64 -d assets/update/keys.txt > dbento-update.pub
   base64 -d latest.json.sig > latest.json.minisig
   minisign -V -p dbento-update.pub -m latest.json -x latest.json.minisig
   ```

3. Mount the `.dmg` and run `codesign -v` on the `.app`.
4. Check that the Windows installer and CLI zip carry their plain versioned names.
5. Install the `.deb` on stock Ubuntu 24.04; it installs `/usr/bin/dbento` and `/usr/bin/dbento-app`.
6. Check that each package and CLI archive holds `THIRD-PARTY-NOTICES.html`, and that the release carries no loose copy.
7. Edit the notes: replace the intro placeholder with one to three sentences on this release's headline changes, and reword What's Changed where it helps.
8. Check the placeholder is gone; this must print `0`:

   ```sh
   gh release view v<version> --json body -q .body | grep -c '<!-- intro:'
   ```

9. Publish the release in the GitHub UI.

## First launch

On macOS, move `Dbento.app` to `/Applications` before the first launch, then run `xattr -dr com.apple.quarantine /Applications/Dbento.app`. Launching from the download folder runs the app from a temporary translocated path, and the MCP connect command the app shows would name that path; from `/Applications` it names a stable one.

The AppImage cannot put its inner `dbento` on `PATH`, so AppImage users also take the CLI archive for their architecture to get `dbento`. The `.deb` installs `dbento` next to the app. The `.dmg` and the Windows installer are built to bundle it too; the first rehearsal is what confirms it.

## Actions minutes

Each job is billed in whole minutes; Windows bills 2x and macOS 10x. No release run has been measured yet, so every figure below is ASSUMED until the first rehearsal records real ones:

- ASSUMED: a CI run on `master` is about 20 to 30 billed minutes.
- ASSUMED: a full release is about 560: Linux about 60, Windows 30 x 2, macOS 2 x 22 x 10.
- ASSUMED: the default `linux,windows,macos-arm64` rehearsal is about 340.

Every job's `timeout-minutes` caps what a hung run can cost: `guard` 5, `build` 90, `checksums` 45, `attest` 15, `draft` 30, and the CI `gate` 60. Release jobs restore no cache, so every build compiles from scratch.
