# Shared POSIX package preparation. Sourced by the Linux and macOS packagers.

prepare_package() {
    case "$version" in
        [0-9]*) ;;
        *) echo "Invalid release version" >&2; exit 1;;
    esac
    case "$version" in
        *[!0-9A-Za-z.+-]*) echo "Invalid release version" >&2; exit 1;;
    esac
    for file in rterm rterm-agent; do
        if [ ! -f "$bin_dir/$file" ] || [ ! -x "$bin_dir/$file" ]; then
            printf 'Missing release executable: %s\n' "$bin_dir/$file" >&2
            exit 1
        fi
    done
    notices="$here/../../target/release-notices/THIRD-PARTY-LICENSES.html"
    if [ ! -s "$notices" ]; then
        echo "Generate third-party license notices before packaging (see docs/USAGE.md)." >&2
        exit 1
    fi
    mkdir -p "$out"
    stage=$(mktemp -d)
    trap 'rm -rf "$stage"' EXIT
}

install_notices() {
    mkdir -p "$1"
    install -m 644 "$here/../../assets/Inter-OFL.txt" "$1/Inter-OFL.txt"
    install -m 644 "$here/../../assets/JetBrainsMono-OFL.txt" "$1/JetBrainsMono-OFL.txt"
    install -m 644 "$here/../../vendor/alacritty_terminal/LICENSE-APACHE" "$1/alacritty-LICENSE-APACHE"
    install -m 644 "$notices" "$1/THIRD-PARTY-LICENSES.html"
    if [ -f "$here/../../LICENSE" ]; then
        install -m 644 "$here/../../LICENSE" "$1/RTerm-LICENSE"
    fi
}
