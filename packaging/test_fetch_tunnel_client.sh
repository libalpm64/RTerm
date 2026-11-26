#!/usr/bin/env bash
# Isolated tests: no network requests and no real installations.
set -euo pipefail
here=$(cd -- "$(dirname -- "$0")" && pwd)
fetch=$here/fetch_tunnel_client.sh
root=$(mktemp -d)
trap 'rm -rf -- "$root"' EXIT
export FIXTURE_ROOT=$root
mkdir "$root/bin" "$root/archives"
unset RTERM_TUNNEL_MANIFEST
cat > "$root/bin/curl" <<'MOCK'
#!/usr/bin/env bash
set -eu
out=
url=
while [[ $# -gt 0 ]]; do
    case "$1" in
        -o) out=$2; shift 2;;
        https://*) url=$1; shift;;
        *) shift;;
    esac
done
printf '%s\n' "$url" >> "$FIXTURE_ROOT/calls"
case "$url" in
    https://api.github.com/repos/openai/tunnel-client/releases/latest)
        cat "$FIXTURE_ROOT/release.json";;
    https://github.com/openai/tunnel-client/releases/download/*)
        cp "$FIXTURE_ROOT/archives/${url##*/}" "$out";;
    *) exit 1;;
esac
MOCK
chmod +x "$root/bin/curl"
export PATH="$root/bin:$PATH"
targets=(linux-amd64 windows-amd64 darwin-amd64 darwin-arm64)
fixture() {
    local version=$1 target asset executable digest dir entry
    jq -n --arg v "$version" '{tag_name:$v,draft:false,prerelease:false,assets:[]}' > "$root/release.json"
    for target in "${targets[@]}"; do
        asset=tunnel-client-$version-$target
        executable=tunnel-client
        [[ $target != windows-amd64 ]] || executable=tunnel-client.exe
        dir="$root/$asset"
        mkdir "$dir"
        for entry in "$executable" LICENSE NOTICE "$asset-licenses.txt" "$asset.spdx.json"; do
            printf 'fixture\n' > "$dir/$entry"
        done
        (cd "$dir"; zip -q "$root/archives/$asset.zip" ./*)
        digest=$(sha256sum < "$root/archives/$asset.zip")
        jq --arg name "$asset.zip" --arg digest "sha256:${digest%% *}" \
            --arg url "https://github.com/openai/tunnel-client/releases/download/$version/$asset.zip" \
            '.assets += [{name:$name,digest:$digest,browser_download_url:$url}]' "$root/release.json" > "$root/new.json"
        mv "$root/new.json" "$root/release.json"
    done
}
reject() {
    if "$@" > "$root/rejected.log" 2>&1; then
        echo "Unexpected success: $*" >&2
        exit 1
    fi
}

fixture v0.0.99
bash "$fetch" --resolve "$root/old.json"
fixture v0.1.0
bash "$fetch" --resolve "$root/latest.json"
[[ $(jq -r .version "$root/old.json") == v0.0.99 ]]
[[ $(jq -r .version "$root/latest.json") == v0.1.0 ]]
[[ $(grep -c '^https://api.github.com/' "$root/calls") == 2 ]]
cp "$root/release.json" "$root/valid.json"
for filter in '.assets |= map(select(.name | contains("darwin-arm64") | not))' \
    '.assets[0].digest = null' '.assets[0].browser_download_url = "https://example.org/other.zip"' \
    '.prerelease = true' '.draft = true' '.tag_name = "../invalid"'; do
    jq "$filter" "$root/valid.json" > "$root/release.json"
    reject bash "$fetch" --resolve "$root/rejected.json"
    [[ ! -e $root/rejected.json ]]
done
# A saved snapshot must drive every platform, even if upstream changes or fails.
fixture v0.2.0
export RTERM_TUNNEL_MANIFEST=$root/latest.json
: > "$root/calls"
for target in "${targets[@]}"; do
    executable=tunnel-client
    [[ $target != windows-amd64 ]] || executable=tunnel-client.exe
    bash "$fetch" "$target" "$root/output/$target"
    [[ -x $root/output/$target/$executable ]]
    [[ $(cat "$root/output/$target/$executable") == fixture ]]
    [[ $(find "$root/output/$target/licenses" -type f | wc -l) -eq 4 ]]
done
[[ $(wc -l < "$root/calls") -eq 4 ]]
[[ $(grep -c '/releases/download/v0.1.0/' "$root/calls") -eq 4 ]]
printf 'corrupt\n' > "$root/corrupt.zip"
reject bash "$fetch" linux-amd64 "$root/rejected" "$root/corrupt.zip"
grep -q 'SHA-256 mismatch' "$root/rejected.log"
[[ ! -e $root/rejected ]]
echo 'Tunnel release selection and checksum tests passed.'
