#!/bin/sh
# Installs the latest openrecall release binary at ~/.local/bin/openrecall, run by the install command in README.md.
# The last line calls the one function, so a download cut short runs nothing.

install_openrecall() {
    releases=https://github.com/rajnandan1/openrecall/releases
    asset=openrecall-aarch64-apple-darwin
    bin="$HOME/.local/bin"

    fail() {
        echo "OpenRecall install failed: $1" >&2
        exit 1
    }

    if [ "$(uname -sm)" != "Darwin arm64" ]; then
        echo "OpenRecall has a release only for macOS on Apple silicon. To build from source, see https://github.com/rajnandan1/openrecall#build-from-source" >&2
        exit 1
    fi
    mkdir -p "$bin" || fail "cannot create $bin"
    find "$bin" -maxdepth 1 -name '.openrecall-install.*' -mmin +10 -exec rm -rf {} +

    to=$(/usr/bin/curl -q -sS --proto =https --max-time 10 -o /dev/null -w '%{redirect_url}' "$releases/latest") ||
        fail "cannot reach $releases/latest"
    case "$to" in
        */releases/tag/v*) version=${to##*/releases/tag/v} ;;
        "") fail "no redirect from $releases/latest" ;;
        *) fail "no release at $releases" ;;
    esac
    echo "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || fail "the tag of the latest release does not parse: $to"

    tmp=$(mktemp -d "$bin/.openrecall-install.XXXXXX") || fail "cannot create a temporary folder in $bin"
    trap 'rm -rf "$tmp"' EXIT
    trap 'exit 1' HUP INT TERM
    cd "$tmp" || fail "cannot enter $tmp"
    /usr/bin/curl -q -fsSL --proto =https --max-time 10 -o "$asset.sha256" "$releases/download/v$version/$asset.sha256" ||
        fail "cannot download $asset.sha256 of v$version"
    /usr/bin/curl -q -fsSL --proto =https --max-time 120 -o "$asset" "$releases/download/v$version/$asset" ||
        fail "cannot download $asset of v$version"
    /usr/bin/shasum -a 256 -c "$asset.sha256" >/dev/null || fail "the download does not match the SHA-256 hash of v$version"
    chmod +x "$asset" || fail "cannot make the download executable"
    [ "$(./"$asset" version)" = "$version" ] || fail "the download does not print its version $version"
    mv -f "$asset" "$bin/openrecall" || fail "cannot move the binary to $bin/openrecall"

    echo "Installed openrecall $version at ~/.local/bin/openrecall"
    case ":$PATH:" in
        *":$bin:"*) ;;
        *) echo '~/.local/bin is not on your PATH, so Claude Code cannot find openrecall. Add this line to ~/.zshrc and open a new terminal: export PATH="$HOME/.local/bin:$PATH"' ;;
    esac
}

install_openrecall
