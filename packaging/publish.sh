#!/usr/bin/env bash
set -euo pipefail

# This runs only after every native installer job and the audit have passed.
# Repository Settings > General > Releases must enable release immutability.
: "${GITHUB_SHA:?Missing commit SHA}"
: "${GITHUB_REPOSITORY:?Missing repository}"
: "${GITHUB_RUN_ID:?Missing workflow run ID}"
[[ "$GITHUB_SHA" =~ ^[0-9a-f]{40}$ ]]
[[ "$(git rev-parse HEAD)" == "$GITHUB_SHA" ]]
[[ "${GITHUB_REF:-}" == refs/heads/main ]]
version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -1)
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
tag="rterm-$GITHUB_SHA"
title="RTerm #${GITHUB_SHA:0:12}"

# Check every expected installer and its original checksum before publishing.
# Keep platform package names and embedded version numbers unchanged.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
staging="$work/assets"
mkdir "$staging"
packages=(
    "RTerm-windows-x64/RTerm-$version-windows-x64-setup.exe"
    "RTerm-macos-arm64/RTerm-$version-macos-arm64.pkg"
    "RTerm-macos-x64/RTerm-$version-macos-x64.pkg"
    "RTerm-linux-amd64/RTerm-$version-linux-amd64.deb"
    "RTerm-linux-amd64/RTerm-$version-linux-amd64.tar.gz"
)
for package in "${packages[@]}"; do
    file="release-assets/$package"
    name=${file##*/}
    if [[ ! -s "$file" || ! -s "$file.sha256" ]]; then
        echo "Missing installer or checksum: $package" >&2
        exit 1
    fi
    read -r expected recorded < "$file.sha256"
    # Windows PowerShell writes checksum files with CRLF line endings.
    recorded=${recorded%$'\r'}
    actual=$(sha256sum "$file")
    if [[ ! "$expected" =~ ^[0-9a-f]{64}$ || "$recorded" != "$name" || "${actual%% *}" != "$expected" ]]; then
        echo "Invalid checksum: $package" >&2
        exit 1
    fi
    cp "$file" "$staging/$name"
    printf '%s  %s\n' "$expected" "$name" > "$staging/$name.sha256"
done
[[ -s release-notices/THIRD-PARTY-LICENSES.html ]]
cp release-notices/THIRD-PARTY-LICENSES.html "$staging/"
printf '%s\n' "$GITHUB_SHA" > "$staging/COMMIT.txt"
(
    cd "$staging"
    sha256sum -- *.exe *.pkg *.deb *.tar.gz THIRD-PARTY-LICENSES.html COMMIT.txt > SHA256SUMS.txt
    sha256sum --check SHA256SUMS.txt
)

# Never overwrite or delete a release, asset, or tag. An existing release
# is a no-op only if it is already immutable and tied to this exact commit.
if existing=$(gh release view "$tag" --repo "$GITHUB_REPOSITORY" --json databaseId --jq .databaseId 2>/dev/null); then
    gh api "repos/$GITHUB_REPOSITORY/releases/$existing" --jq '.immutable and (.draft | not)' | grep -qx true
    gh api "repos/$GITHUB_REPOSITORY/git/ref/tags/$tag" --jq '.object.sha' | grep -Fxq "$GITHUB_SHA"
    echo "Commit already published as an immutable release."
    exit 0
fi

# Use a lightweight tag explicitly pointing at the checked-out commit.
# If the tag already exists, refuse to move it (including on reruns).
gh api --method POST "repos/$GITHUB_REPOSITORY/git/refs" \
    -f "ref=refs/tags/$tag" -f "sha=$GITHUB_SHA" >/dev/null
cat > "$work/notes.txt" <<EOF
RTerm V2 ($version)

Commit: https://github.com/$GITHUB_REPOSITORY/commit/$GITHUB_SHA
Build: https://github.com/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID

Windows x64 | macOS Apple Silicon and Intel | Linux amd64

All native build and test jobs and the dependency audit passed.
Windows installers are unsigned. macOS packages are ad-hoc signed and not notarized.
Linux packages target Ubuntu 24.04 or compatible systems.

Verify downloads using SHA256SUMS.txt.
EOF

# Upload to a draft before publishing so immutable assets are complete.
gh release create "$tag" "$staging"/* \
    --repo "$GITHUB_REPOSITORY" --verify-tag --draft --title "$title" --notes-file "$work/notes.txt"
id=$(gh release view "$tag" --repo "$GITHUB_REPOSITORY" --json databaseId --jq .databaseId)
[[ "$id" =~ ^[0-9]+$ ]]
# Check that every uploaded asset (including checksum files) has the exact
# local digest before making the draft public.
expected_assets=$(
    for file in "$staging"/*; do
        hash=$(sha256sum "$file")
        printf '%s\tsha256:%s\n' "${file##*/}" "${hash%% *}"
    done | LC_ALL=C sort
)
uploaded_assets=$(gh api "repos/$GITHUB_REPOSITORY/releases/$id" \
    --jq '.assets[] | select(.state == "uploaded") | [.name, .digest] | @tsv' | LC_ALL=C sort)
if [[ "$uploaded_assets" != "$expected_assets" ]]; then
    echo "Draft asset verification failed; release has not been published." >&2
    exit 1
fi
gh api "repos/$GITHUB_REPOSITORY/git/ref/tags/$tag" --jq '.object.sha' | grep -Fxq "$GITHUB_SHA"
gh release edit "$tag" --repo "$GITHUB_REPOSITORY" --draft=false
# GitHub sets the immutable flag at publication. Never claim success without it.
if ! gh api "repos/$GITHUB_REPOSITORY/releases/$id" --jq '.immutable and (.draft | not)' | grep -qx true; then
    echo "Release immutability was not confirmed. Check repository settings." >&2
    exit 1
fi
gh api "repos/$GITHUB_REPOSITORY/git/ref/tags/$tag" --jq '.object.sha' | grep -Fxq "$GITHUB_SHA"
