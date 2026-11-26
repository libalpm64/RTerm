#!/bin/sh
set -eu
bin_dir=${1:?Release binary directory}
version=${2:?Version}
arch=${3:?Architecture}
out=${4:-dist}
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# shellcheck source=packaging/common.sh
. "$here/../common.sh"
prepare_package
case "$arch" in arm64) cpu=arm64;; x64) cpu=x86_64;; *) echo "Unsupported macOS architecture" >&2; exit 1;; esac
for file in rterm rterm-agent; do
    lipo -verify_arch "$cpu" "$bin_dir/$file"
done
root="$stage/root"
app="$root/Applications/RTerm.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$root/usr/local/bin"
install -m 755 "$bin_dir/rterm" "$app/Contents/MacOS/rterm"
install -m 755 "$bin_dir/rterm-agent" "$app/Contents/MacOS/rterm-agent"
cp "$here/Info.plist" "$app/Contents/Info.plist"
install -m 644 "$here/../../assets/rterm.icns" "$app/Contents/Resources/rterm.icns"
install_notices "$app/Contents/Resources/licenses"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" "$app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $version" "$app/Contents/Info.plist"
ln -s /Applications/RTerm.app/Contents/MacOS/rterm-agent "$root/usr/local/bin/rterm-agent"
# A signed build is all-or-nothing. Identities and the notary keychain profile
# are configured externally, never embedded in the repository or command log.
if [ "${RTERM_SIGN:-0}" = 1 ]; then
    : "${RTERM_MAC_APP_IDENTITY:?Developer ID Application identity required}"
    : "${RTERM_MAC_INSTALLER_IDENTITY:?Developer ID Installer identity required}"
    : "${RTERM_MAC_NOTARY_PROFILE:?notarytool keychain profile required}"
    codesign --force --options runtime --timestamp --sign "$RTERM_MAC_APP_IDENTITY" "$app/Contents/MacOS/rterm-agent"
    codesign --force --options runtime --timestamp --sign "$RTERM_MAC_APP_IDENTITY" "$app"
else
    if [ "${RTERM_SIGN:-0}" != 0 ]; then
        echo "RTERM_SIGN must be 0 or 1" >&2; exit 1
    fi
    echo "Creating ad-hoc signed test package (not notarized)." >&2
    codesign --force --sign - "$app/Contents/MacOS/rterm-agent"
    codesign --force --sign - "$app"
fi
codesign --verify --deep --strict "$app"
pkgbuild --analyze --root "$root" "$stage/components.plist"
/usr/libexec/PlistBuddy -c 'Set :0:BundleIsRelocatable false' "$stage/components.plist"
set -- --root "$root" --component-plist "$stage/components.plist" --install-location / \
  --identifier org.rterm.desktop --version "$version"
if [ "${RTERM_SIGN:-0}" = 1 ]; then
    set -- "$@" --sign "$RTERM_MAC_INSTALLER_IDENTITY"
fi
package="$stage/RTerm-$version-macos-$arch.pkg"
pkgbuild "$@" "$package"
if [ "${RTERM_SIGN:-0}" = 1 ]; then
    pkgutil --check-signature "$package"
    xcrun notarytool submit "$package" --keychain-profile "$RTERM_MAC_NOTARY_PROFILE" --wait
    xcrun stapler staple "$package"
    xcrun stapler validate "$package"
    spctl --assess --type install --verbose "$package"
fi
mv "$package" "$out/"
(cd "$out" && shasum -a 256 "${package##*/}" > "${package##*/}.sha256")
