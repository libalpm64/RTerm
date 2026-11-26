#!/usr/bin/env bash
# Resolve the latest stable release once per build, then stage verified assets.
set -euo pipefail
export LC_ALL=C
repository=https://github.com/openai/tunnel-client
latest=https://api.github.com/repos/openai/tunnel-client/releases/latest

fail() { printf '%s\n' "$*" >&2; exit 1; }
download() {
    curl --proto '=https' --proto-redir '=https' --fail --location \
        --silent --show-error --retry 3 --connect-timeout 15 --max-time 180 "$@"
}
release_metadata() {
    # Authenticate only the fixed GitHub API endpoint, never asset downloads.
    # Read the header from stdin so the token is not a curl command argument.
    local -a args=(--max-redirs 0 --header 'Accept: application/vnd.github+json'
        --header 'X-GitHub-Api-Version: 2022-11-28' --header 'Cache-Control: no-cache')
    if [[ -n ${GH_TOKEN:-} ]]; then
        printf 'Authorization: Bearer %s\n' "$GH_TOKEN" |
            download "${args[@]}" --header @- "$latest"
    else
        download "${args[@]}" "$latest"
    fi
}
resolve() {
    local metadata
    if ! metadata=$(release_metadata); then
        fail "GitHub release lookup failed. Check API access and rate limits; set GH_TOKEN for authenticated requests."
    fi
    printf '%s\n' "$metadata" |
        jq -e --arg repository "$repository" '
        if .draft != false or .prerelease != false or
            (.tag_name | type != "string" or (test("^v[0-9]+\\.[0-9]+\\.[0-9]+$") | not))
        then error("Expected a published stable release") else . end
        | . as $release
        | {version: .tag_name, digests: (
            reduce ["linux-amd64","linux-arm64","windows-amd64","darwin-amd64","darwin-arm64"][] as $target ({};
                ("tunnel-client-" + $release.tag_name + "-" + $target + ".zip") as $name
                | [$release.assets[] | select(.name == $name)] as $assets
                | if ($assets | length) == 0 and $target == "linux-arm64" then .
                  elif ($assets | length) != 1 then error("Missing or duplicate upstream asset: " + $name)
                  elif $assets[0].browser_download_url != ($repository + "/releases/download/" + $release.tag_name + "/" + $name)
                  then error("Unexpected upstream URL: " + $name)
                  elif ($assets[0].digest | type != "string" or (test("^sha256:[0-9a-f]{64}$") | not))
                  then error("Missing published SHA-256: " + $name)
                  else .[$target] = ($assets[0].digest | ltrimstr("sha256:"))
                  end)
        )}'
}

if [[ ${1:-} == --resolve ]]; then
    [[ $# == 2 ]] || fail "Usage: $0 --resolve manifest.json"
    manifest=$(resolve)
    mkdir -p "$(dirname "$2")"
    printf '%s\n' "$manifest" > "$2"
    printf 'Resolved OpenAI tunnel-client %s\n' "$(jq -r .version <<< "$manifest")"
    exit 0
fi

[[ $# -ge 2 && $# -le 3 ]] || fail "Usage: $0 target destination [archive.zip]"
target=$1
destination=$2
archive=${3:-}
if command -v cygpath >/dev/null 2>&1; then
    destination=$(cygpath -u "$destination")
    if [[ -n $archive ]]; then archive=$(cygpath -u "$archive"); fi
    if [[ -n ${RTERM_TUNNEL_MANIFEST:-} ]]; then
        RTERM_TUNNEL_MANIFEST=$(cygpath -u "$RTERM_TUNNEL_MANIFEST")
    fi
fi
case "$target" in
    linux-amd64|linux-arm64|darwin-amd64|darwin-arm64) executable=tunnel-client;;
    windows-amd64) executable=tunnel-client.exe;;
    *) fail "Unsupported tunnel-client target: $target";;
esac
if [[ -n ${RTERM_TUNNEL_MANIFEST:-} ]]; then
    manifest=$(cat -- "$RTERM_TUNNEL_MANIFEST")
else
    manifest=$(resolve)
fi
version=$(jq -er '.version | select(type == "string" and test("^v[0-9]+\\.[0-9]+\\.[0-9]+$"))' <<< "$manifest")
digest=$(jq -er --arg target "$target" '.digests[$target] | select(type == "string" and test("^[0-9a-f]{64}$"))' <<< "$manifest")
asset="tunnel-client-$version-$target"

stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
if [[ -z $archive ]]; then
    archive="$stage/$asset.zip"
    download -o "$archive" "$repository/releases/download/$version/$asset.zip"
fi
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum < "$archive")
else
    actual=$(shasum -a 256 < "$archive")
fi
[[ ${actual%% *} == "$digest" ]] || fail "SHA-256 mismatch for $asset"

# Extract only named files into a fresh directory; never unpack archive paths.
for entry in "$executable" LICENSE NOTICE "$asset-licenses.txt" "$asset.spdx.json"; do
    unzip -p "$archive" "$entry" > "$stage/$entry"
    [[ -s $stage/$entry ]] || fail "Empty archive entry: $entry"
done
mkdir -p "$destination/licenses"
install -m 755 "$stage/$executable" "$destination/$executable"
install -m 644 "$stage/LICENSE" "$destination/licenses/tunnel-client-LICENSE"
install -m 644 "$stage/NOTICE" "$destination/licenses/tunnel-client-NOTICE"
install -m 644 "$stage/$asset-licenses.txt" "$destination/licenses/tunnel-client-THIRD-PARTY-LICENSES.txt"
install -m 644 "$stage/$asset.spdx.json" "$destination/licenses/tunnel-client.spdx.json"
printf 'Staged verified OpenAI tunnel-client %s (%s)\n' "$version" "$target"
