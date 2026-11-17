<!-- intro: one to three sentences on this release's headline changes; replace this line before publishing -->

## What's Changed

{changes}

## Downloads

| Platform | File |
| --- | --- |
| Linux x86_64, app | [dbento-app_{version}_x86_64.AppImage](https://github.com/PhongGuy/dbento/releases/download/v{version}/dbento-app_{version}_x86_64.AppImage) for most Linux systems, or [dbento-app_{version}_amd64.deb](https://github.com/PhongGuy/dbento/releases/download/v{version}/dbento-app_{version}_amd64.deb) for Ubuntu and Debian |
| Linux x86_64, command line | [dbento-{version}-x86_64-unknown-linux-gnu.tar.gz](https://github.com/PhongGuy/dbento/releases/download/v{version}/dbento-{version}-x86_64-unknown-linux-gnu.tar.gz) |
| Linux arm64, app | [dbento-app_{version}_aarch64.AppImage](https://github.com/PhongGuy/dbento/releases/download/v{version}/dbento-app_{version}_aarch64.AppImage) for most Linux systems, or [dbento-app_{version}_arm64.deb](https://github.com/PhongGuy/dbento/releases/download/v{version}/dbento-app_{version}_arm64.deb) for Ubuntu and Debian |
| Linux arm64, command line | [dbento-{version}-aarch64-unknown-linux-gnu.tar.gz](https://github.com/PhongGuy/dbento/releases/download/v{version}/dbento-{version}-aarch64-unknown-linux-gnu.tar.gz) |
| macOS Apple silicon, app | [Dbento_{version}_aarch64.dmg](https://github.com/PhongGuy/dbento/releases/download/v{version}/Dbento_{version}_aarch64.dmg) |
| macOS Apple silicon, command line | [dbento-{version}-aarch64-apple-darwin.tar.gz](https://github.com/PhongGuy/dbento/releases/download/v{version}/dbento-{version}-aarch64-apple-darwin.tar.gz) |
| macOS Intel, app | [Dbento_{version}_x86_64.dmg](https://github.com/PhongGuy/dbento/releases/download/v{version}/Dbento_{version}_x86_64.dmg) |
| macOS Intel, command line | [dbento-{version}-x86_64-apple-darwin.tar.gz](https://github.com/PhongGuy/dbento/releases/download/v{version}/dbento-{version}-x86_64-apple-darwin.tar.gz) |
| Windows x64, app | [dbento-app_{version}_x64-setup.exe](https://github.com/PhongGuy/dbento/releases/download/v{version}/dbento-app_{version}_x64-setup.exe) |
| Windows x64, command line | [dbento-{version}-x86_64-pc-windows-msvc.zip](https://github.com/PhongGuy/dbento/releases/download/v{version}/dbento-{version}-x86_64-pc-windows-msvc.zip) |

Each file also has a copy with no version in its name, for links that always point at the latest release. Check a download with `sha256sum -c SHA256SUMS`. To check that a file was built by GitHub from this project's own code, use `gh attestation verify <file> --repo PhongGuy/dbento`. Third-party licences are in `THIRD-PARTY-NOTICES.html` inside every package.

## Before you install

- **macOS:** Dbento is free and not notarized by Apple, so macOS blocks its first launch. Drag Dbento to Applications, then run `xattr -dr com.apple.quarantine /Applications/Dbento.app` once before opening it.
- **Windows:** Windows may show a blue box saying "Windows protected your PC". Dbento is free and not signed with a paid certificate, so Windows does not recognise it yet. Click "More info", then "Run anyway". Only do this for the file from this page.
- **Linux AppImage:** after downloading, right-click the file, open Properties, allow it to run as a program, then double-click it. The AppImage holds only the desktop app; take the command-line archive for your architecture to get `dbento`.
- **Linux .deb:** open it with your software installer and click Install. It installs both `dbento-app` and the `dbento` command.

## Updates

Dbento checks GitHub once a day for a newer release and tells you when one is out. Nothing downloads or installs until you choose to update, and every update is verified against a signature before it is applied. To turn the check off, clear **Check for updates** in Settings. You can always get new versions from the [Releases page](https://github.com/PhongGuy/dbento/releases).

On macOS, the first launch after an update asks for keychain access once for each saved connection password, because the updated app is a new build. Choose "Always Allow" to keep it from asking again.
