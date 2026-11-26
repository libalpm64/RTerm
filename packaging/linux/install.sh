#!/bin/sh
# Install the desktop, CLI and tunnel client from the Linux portable archive.
set -eu
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
for file in rterm rterm-agent tunnel-client rterm.desktop rterm.png licenses/Inter-OFL.txt licenses/JetBrainsMono-OFL.txt licenses/alacritty-LICENSE-APACHE licenses/THIRD-PARTY-LICENSES.html licenses/tunnel-client-LICENSE licenses/tunnel-client-NOTICE licenses/tunnel-client-THIRD-PARTY-LICENSES.txt licenses/tunnel-client.spdx.json; do
    if [ ! -f "$here/$file" ] || [ ! -r "$here/$file" ]; then
        printf 'Incomplete RTerm archive: missing %s\n' "$file" >&2
        exit 1
    fi
done
uid=$(id -u)
# Register the app in the XDG application menu even for a custom prefix.
# A graphical session need not inherit the same PATH as the install shell.
if [ "$uid" = 0 ]; then
    prefix=${PREFIX:-/usr/local}
    data_home=/usr/local/share
else
    prefix=${PREFIX:-"$HOME/.local"}
    data_home=${XDG_DATA_HOME:-"$HOME/.local/share"}
    case "$data_home" in /*) ;; *) data_home="$HOME/.local/share";; esac
fi
case "$prefix" in /*) ;; *) echo "PREFIX must be absolute" >&2; exit 1;; esac
# grep processes newline-separated records, so reject embedded newlines first.
case "$prefix" in
    *'
'*) echo "PREFIX must not contain newlines" >&2; exit 1;;
esac
if printf '%s' "$prefix" | LC_ALL=C grep -q '[[:cntrl:]]'; then
    echo "PREFIX must not contain control characters" >&2
    exit 1
fi
mkdir -p "$prefix/bin" "$prefix/lib/rterm" "$prefix/share/applications"
install -m 755 -T "$here/rterm" "$prefix/bin/rterm"
install -m 755 -T "$here/rterm-agent" "$prefix/bin/rterm-agent"
install -m 755 -T "$here/tunnel-client" "$prefix/lib/rterm/tunnel-client"
# Escape Exec's quoted argument and then the desktop-entry string value.
exec_path=$(printf '%s' "$prefix/bin/rterm" | sed 's/%/%%/g; s/["`$\\]/\\&/g; s/\\/\\\\/g')
exec_line="Exec=\"$exec_path\""
# GLib checks the executable before expanding literal percent escapes.
# The spec also disallows '=' in the executable token. For these paths,
# pass the filename as $0 to a constant shell command, never as shell code.
case "$prefix" in
    *%*|*=*)
        # The desktop launcher must pass a literal $0 to the child shell.
        # shellcheck disable=SC2016
        exec_line=$(printf '%s "%s"' 'Exec=/bin/sh -c "exec \\"\\$0\\""' "$exec_path");;
esac
stage=$(mktemp -d)
trap 'rm -f "$stage/rterm.desktop"; rmdir "$stage"' EXIT
{
    # Keep the group header first; use printf for the path, not a sed
    # replacement expression (custom paths may contain & or backslashes).
    while IFS= read -r line; do
        case "$line" in Exec=*) printf '%s\n' "$exec_line";; *) printf '%s\n' "$line";; esac
    done < "$here/rterm.desktop"
} > "$stage/rterm.desktop"
mkdir -p "$data_home/applications"
icon_dir="$data_home/icons/hicolor/256x256/apps"
mkdir -p "$icon_dir"
install -m 644 -T "$here/rterm.png" "$icon_dir/rterm.png"
doc_dir="$prefix/share/doc/rterm"
mkdir -p "$doc_dir"
for file in Inter-OFL.txt JetBrainsMono-OFL.txt alacritty-LICENSE-APACHE THIRD-PARTY-LICENSES.html tunnel-client-LICENSE tunnel-client-NOTICE tunnel-client-THIRD-PARTY-LICENSES.txt tunnel-client.spdx.json; do
    install -m 644 -T "$here/licenses/$file" "$doc_dir/$file"
done
if [ -f "$here/licenses/RTerm-LICENSE" ]; then
    install -m 644 -T "$here/licenses/RTerm-LICENSE" "$doc_dir/RTerm-LICENSE"
fi
# Install from staging rather than redirecting into an existing launcher.
# Keep the prefix copy for installed-build detection, and register the same
# desktop ID in XDG. Compare directories by identity, not spelling.
install -m 644 -T "$stage/rterm.desktop" "$prefix/share/applications/rterm.desktop"
prefix_identity=$(stat -Lc '%d:%i' -- "$prefix/share/applications")
data_identity=$(stat -Lc '%d:%i' -- "$data_home/applications")
if [ "$prefix_identity" != "$data_identity" ]; then
    install -m 644 -T "$stage/rterm.desktop" "$data_home/applications/rterm.desktop"
fi
echo "Installed RTerm and rterm-agent in $prefix/bin; OpenAI tunnel-client in $prefix/lib/rterm."
if ! command -v fc-match >/dev/null 2>&1; then
    echo "Install Fontconfig and Noto Sans CJK for automatic Chinese, Japanese and Korean font discovery."
fi
case ":$PATH:" in *":$prefix/bin:"*) ;; *) echo "Add $prefix/bin to PATH and restart your terminal/AI client.";; esac
# Ask only in an interactive terminal, as the user who owns the desktop.
# Noninteractive installs and elevated installs defer to the app on first launch.
if [ -t 0 ] && [ "$uid" != 0 ]; then
    printf 'Create an RTerm desktop shortcut? [y/N] '
    if IFS= read -r answer; then
        case "$answer" in y|Y|yes|Yes|YES) choice=yes;; *) choice=no;; esac
        if ! "$prefix/bin/rterm" --desktop-shortcut "$choice"; then
            echo "Shortcut setup unavailable; RTerm is installed. Setup can be offered on first launch."
        fi
    fi
fi
