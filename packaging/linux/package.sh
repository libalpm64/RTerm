#!/bin/sh
set -eu
bin_dir=${1:?Release binary directory}
version=${2:?Version}
out=${3:-dist}
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
# shellcheck source=packaging/common.sh
. "$here/../common.sh"
prepare_package
root="$stage/root"
arch=$(dpkg --print-architecture)
# Bundle a verified client at package time; installations never need network access.
bash "$here/../fetch_tunnel_client.sh" "linux-$arch" "$stage/tunnel" "${RTERM_TUNNEL_ARCHIVE:-}"
"$stage/tunnel/tunnel-client" --version
icon_dir=share/icons/hicolor/256x256/apps
mkdir -p "$root/DEBIAN" "$root/usr/bin" "$root/usr/share/applications" "$root/usr/$icon_dir"
install -m 755 "$bin_dir/rterm" "$root/usr/bin/rterm"
install -m 755 "$bin_dir/rterm-agent" "$root/usr/bin/rterm-agent"
mkdir -p "$root/usr/lib/rterm"
install -m 755 "$stage/tunnel/tunnel-client" "$root/usr/lib/rterm/tunnel-client"
install -m 644 "$here/rterm.desktop" "$root/usr/share/applications/rterm.desktop"
install -m 644 "$here/../../assets/rterm.png" "$root/usr/$icon_dir/rterm.png"
install_notices "$root/usr/share/doc/rterm"
install -m 644 "$stage/tunnel/licenses/"* "$root/usr/share/doc/rterm/"
cat > "$root/DEBIAN/control" <<EOF
Package: rterm
Version: $version
Architecture: $arch
Maintainer: RTerm contributors
Depends: libc6 (>= 2.39), libgcc-s1, libgl1, libegl1, libxkbcommon0, libx11-6, libwayland-client0, fontconfig, fonts-noto-cjk, ca-certificates
Section: utils
Priority: optional
Description: Native terminal and SSH client with opt-in local AI CLI
EOF
dpkg-deb --root-owner-group --build "$root" "$out/RTerm-$version-linux-$arch.deb"
mkdir -p "$stage/portable"
install -m 755 "$bin_dir/rterm" "$stage/portable/rterm"
install -m 755 "$bin_dir/rterm-agent" "$stage/portable/rterm-agent"
install -m 755 "$stage/tunnel/tunnel-client" "$stage/portable/tunnel-client"
install -m 755 "$here/install.sh" "$stage/portable/install.sh"
install -m 644 "$here/rterm.desktop" "$stage/portable/rterm.desktop"
install -m 644 "$here/../../assets/rterm.png" "$stage/portable/rterm.png"
mkdir -p "$stage/portable/licenses"
install -m 644 "$root/usr/share/doc/rterm/"* "$stage/portable/licenses/"
tar -C "$stage/portable" -czf "$out/RTerm-$version-linux-$arch.tar.gz" .
(cd "$out" && for file in "RTerm-$version-linux-$arch.deb" "RTerm-$version-linux-$arch.tar.gz"; do
    sha256sum "$file" > "$file.sha256"
done)
