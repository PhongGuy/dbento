#!/usr/bin/env bash
# Installs or uninstalls dbento and dbento-app for this OS: `install.sh install|uninstall`.
# DBENTO_PREFIX becomes cargo's --root; JOBS caps the build; DBENTO_INSTALL_SKIP_CARGO=1 skips
# cargo so tests can check the desktop entry alone.
set -euo pipefail

if [ $# -ne 1 ] || { [ "$1" != install ] && [ "$1" != uninstall ]; }; then
    echo "usage: $0 install|uninstall" >&2
    exit 2
fi
action=$1
root=$(cd "$(dirname "$0")/.." && pwd)
cargo=${CARGO:-cargo}
packages=(dbento-cli dbento-app)
skip_cargo=${DBENTO_INSTALL_SKIP_CARGO:-}

# The bin directory cargo installs into, in cargo's own order of precedence.
root_args=()
if [ -n "${DBENTO_PREFIX:-}" ]; then
    prefix=$DBENTO_PREFIX
    case $prefix in /*) ;; *) prefix=$PWD/$prefix ;; esac
    root_args=(--root "$prefix")
    bin=$prefix/bin
elif [ -n "${CARGO_INSTALL_ROOT:-}" ]; then
    bin=$CARGO_INSTALL_ROOT/bin
else
    bin=${CARGO_HOME:-$HOME/.cargo}/bin
fi

case $bin in /*) ;; *) bin=$PWD/$bin ;; esac

# Exec quoted per the desktop entry spec. The string escape doubles each backslash that
# quoting adds, so a backslash becomes four and " ` $ get two in front; % is doubled.
exec_path=$(printf '%s' "$bin/dbento-app" | sed -e 's/\\/\\\\\\\\/g' -e 's/["`$]/\\\\&/g' -e 's/%/%%/g')

data=${XDG_DATA_HOME:-$HOME/.local/share}
desktop=$data/applications/dbento-app.desktop
hicolor=$data/icons/hicolor
# hicolor has no 1024 directory, so the largest committed export is left out.
sizes=(16 24 32 48 64 128 256 512)
# The entry and scalable icon of earlier installs, removed so only one launcher remains.
old_files=("$data/applications/dbento.desktop" "$hicolor/scalable/apps/dbento.svg")

# Cache refreshers are optional and their failure never fails the install.
refresh_caches() {
    if command -v update-desktop-database > /dev/null; then
        update-desktop-database -q "$data/applications" 2> /dev/null || true
    fi
    # Only an existing user cache is refreshed: a new one would hide icons other apps add later.
    if command -v gtk-update-icon-cache > /dev/null && [ -f "$hicolor/icon-theme.cache" ]; then
        gtk-update-icon-cache -q -t "$hicolor" 2> /dev/null || true
    fi
}

os=$(uname -s)

if [ "$action" = install ]; then
    if [ "$os" != Linux ] && [ "$os" != Darwin ]; then
        echo "Unrecognised OS $os; install with:"
        for p in "${packages[@]}"; do
            echo "  cargo install --locked --path crates/$p"
        done
        exit 0
    fi
    if [ -z "$skip_cargo" ]; then
        for p in "${packages[@]}"; do
            CARGO_BUILD_JOBS=${JOBS:-4} "$cargo" install --locked ${root_args[@]+"${root_args[@]}"} \
                --path "$root/crates/$p"
        done
    fi
    if [ "$os" = Darwin ]; then
        echo "Installed dbento and dbento-app in $bin; the .app bundle comes with the release milestone."
        exit 0
    fi
    mkdir -p "$(dirname "$desktop")"
    for n in "${sizes[@]}"; do
        mkdir -p "$hicolor/${n}x$n/apps"
        command cp -f "$root/assets/icon/generated/png/${n}x$n.png" "$hicolor/${n}x$n/apps/dbento-app.png"
    done
    cat > "$desktop" << EOF
[Desktop Entry]
Type=Application
Name=Dbento
Comment=Database client
Exec="$exec_path" %f
Icon=dbento-app
StartupWMClass=dbento-app
Terminal=false
Categories=Development;Database;
EOF
    command rm -f "${old_files[@]}"
    refresh_caches
    echo "Installed dbento and dbento-app in $bin, with a desktop entry at $desktop."
else
    if [ -z "$skip_cargo" ]; then
        # A package that is not installed is not an error here.
        for p in "${packages[@]}"; do
            "$cargo" uninstall ${root_args[@]+"${root_args[@]}"} "$p" 2> /dev/null || true
        done
    fi
    if [ "$os" = Linux ]; then
        command rm -f "$desktop" "${old_files[@]}"
        for n in "${sizes[@]}"; do
            command rm -f "$hicolor/${n}x$n/apps/dbento-app.png"
        done
        refresh_caches
    fi
    echo "Uninstalled dbento."
fi
