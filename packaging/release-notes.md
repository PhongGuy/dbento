<!-- intro: one to three sentences on this release's headline changes; replace this line before publishing -->

## What's Changed

{changes}

## Downloads

| Platform | File |
|---|---|
| Linux x86_64, app | `dbento-app_{version}_amd64.deb` or `dbento-app_{version}_x86_64.AppImage` |
| Linux x86_64, command line | `dbento-{version}-x86_64-unknown-linux-gnu.tar.gz` |
| Linux arm64, app | `dbento-app_{version}_arm64.deb` or `dbento-app_{version}_aarch64.AppImage` |
| Linux arm64, command line | `dbento-{version}-aarch64-unknown-linux-gnu.tar.gz` |
| macOS Apple silicon, app | `Dbento_{version}_aarch64.dmg` |
| macOS Apple silicon, command line | `dbento-{version}-aarch64-apple-darwin.tar.gz` |
| macOS Intel, app | `Dbento_{version}_x86_64.dmg` |
| macOS Intel, command line | `dbento-{version}-x86_64-apple-darwin.tar.gz` |
| Windows x64, app | `dbento-app_{version}_x64-setup-unsigned.exe` |
| Windows x64, command line | `dbento-{version}-x86_64-pc-windows-msvc-unsigned.zip` |

Each file also has a copy with no version in its name, for links that always point at the latest release. Third-party licences are in `THIRD-PARTY-NOTICES.html` inside every package.

Check a download with `sha256sum -c SHA256SUMS`.

## Before you install

These builds are not code-signed yet.

- **macOS:** drag Dbento to Applications, then run `xattr -dr com.apple.quarantine /Applications/Dbento.app` once before the first launch.
- **Windows:** SmartScreen warns about an unknown publisher. Choose "More info", then "Run anyway". The copies with no version in their name (`dbento-app_x64-setup.exe`, `dbento-x86_64-pc-windows-msvc.zip`) are the same unsigned files.

## Updates

Dbento does not update itself yet. Get new versions from the releases page.
