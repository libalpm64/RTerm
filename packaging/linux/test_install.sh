#!/usr/bin/env bash
# Isolated installer smoke test. Never install into the real home or system.
set -euo pipefail
[[ $(id -u) != 0 ]] || { echo "Run as an unprivileged user." >&2; exit 1; }
here=$(cd -- "$(dirname -- "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
root=$(mktemp -d)
trap 'rm -rf -- "$root"' EXIT
archive=$root/archive
home=$root/home
mkdir -p "$archive/licenses" "$home"
cp "$here/install.sh" "$here/rterm.desktop" "$archive/"
cp "$repo/assets/rterm.png" "$archive/"
cp "$repo/assets/Inter-OFL.txt" "$repo/assets/JetBrainsMono-OFL.txt" "$archive/licenses/"
cp "$repo/vendor/alacritty_terminal/LICENSE-APACHE" "$archive/licenses/alacritty-LICENSE-APACHE"
printf '<!doctype html><title>Fixture notices</title>\n' > "$archive/licenses/THIRD-PARTY-LICENSES.html"
for notice in RTerm-LICENSE tunnel-client-LICENSE tunnel-client-NOTICE tunnel-client-THIRD-PARTY-LICENSES.txt tunnel-client.spdx.json; do
    printf 'Fixture notice\n' > "$archive/licenses/$notice"
done
for name in rterm rterm-agent tunnel-client; do
    cat > "$archive/$name" <<'STUB'
#!/bin/sh
printf "launched\n" > "$0.launched"
STUB
    chmod +x "$archive/$name"
done
validate() {
    if command -v desktop-file-validate >/dev/null 2>&1; then
        desktop-file-validate "$1"
    fi
}
run_install() (
    unset PREFIX XDG_DATA_HOME
    export HOME=$home
    [[ -z $1 ]] || export PREFIX=$1
    [[ -z $2 ]] || export XDG_DATA_HOME=$2
    sh "$archive/install.sh" </dev/null
)
reject_install() {
    if run_install "$1" "$2" > "$root/rejected.log" 2>&1; then
        echo "Incomplete/invalid installation succeeded" >&2
        exit 1
    fi
}
check_install() {
    local prefix=$1 data=$2 launcher executable marker notice
    executable=$prefix/bin/rterm
    launcher=$data/applications/rterm.desktop
    marker=$executable.launched
    [[ -x $executable && -x $prefix/bin/rterm-agent && -x $prefix/lib/rterm/tunnel-client ]]
    cmp "$archive/tunnel-client" "$prefix/lib/rterm/tunnel-client"
    for notice in "$archive/licenses/"*; do
        cmp "$notice" "$prefix/share/doc/rterm/${notice##*/}"
    done
    cmp "$launcher" "$prefix/share/applications/rterm.desktop"
    [[ ! -L $launcher && $(stat -c '%a' "$launcher") == 644 ]]
    grep -Fxq 'Icon=rterm' "$launcher"
    cmp "$archive/rterm.png" "$data/icons/hicolor/256x256/apps/rterm.png"
    validate "$launcher"
    rm -f -- "$marker"
    HOME="$home" XDG_DATA_HOME="$data" gio launch "$launcher"
    for _ in {1..100}; do
        [[ ! -f $marker ]] || break
        sleep 0.05
    done
    [[ $(cat "$marker") == launched ]]
}
validate "$archive/rterm.desktop"
run_install "" relative
check_install "$home/.local" "$home/.local/share"
[[ ! -e $home/Desktop ]]
for name in rterm-agent tunnel-client licenses/THIRD-PARTY-LICENSES.html licenses/tunnel-client-NOTICE; do
    mv "$archive/$name" "$archive/$name.saved"
    reject_install "$root/incomplete" ""
    [[ ! -e $root/incomplete ]]
    mv "$archive/$name.saved" "$archive/$name"
done
data=$root/xdg
for name in 'custom space' '100% & $`"\=path'; do
    prefix=$root/$name
    run_install "$prefix" "$data"
    check_install "$prefix" "$data"
done
launcher=$data/applications/rterm.desktop
sentinel=$root/sentinel
printf 'untouched\n' > "$sentinel"
rm -f -- "$launcher"
ln -s "$sentinel" "$launcher"
run_install "$prefix" "$data"
check_install "$prefix" "$data"
[[ $(cat "$sentinel") == untouched ]]
alias=$root/share-alias
ln -s "$prefix/share" "$alias"
run_install "$prefix" "$alias"
check_install "$prefix" "$alias"
reject_install relative "$data"
reject_install "$root/"$'bad\npath' "$data"
echo 'Portable installer and GLib launcher smoke tests passed.'
