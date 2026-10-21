#!/usr/bin/env bash
# Secret-free validation and allowlisted public evidence, after signer cleanup.
set -euo pipefail
set +x
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/macos_stable_signing.sh"
[[ -z "${MACOS_CERTIFICATE_P12_BASE64:-}" && -z "${MACOS_CERTIFICATE_PASSWORD:-}" ]] || {
    echo 'Evidence collection must not receive signing credentials' >&2; exit 1;
}
: "${RUNNER_TEMP:?Missing runner temp directory}"
[[ "${GITHUB_SHA:-}" =~ ^[0-9a-f]{40}$ ]] || exit 1
[[ "$(git -C "$ROOT" rev-parse HEAD)" == "$GITHUB_SHA" ]] || exit 1
case "${TARGET:-}" in
    aarch64-apple-darwin) ARCH=arm64 ;;
    x86_64-apple-darwin) ARCH=x86_64 ;;
    *) exit 1 ;;
esac
[[ "$(< "$RUNNER_TEMP/entropy-signing-cleanup.txt")" == $'temporary_keychain_deleted=yes\nsearch_list_restored=yes' ]] || {
    echo 'Missing successful signing cleanup evidence' >&2; exit 1;
}
DMGS=("$ROOT"/dist/macos/*.dmg)
[[ "${#DMGS[@]}" == 1 && -f "${DMGS[0]}" ]] || exit 1
hdiutil verify "${DMGS[0]}" >/dev/null
WORK="$(mktemp -d "$RUNNER_TEMP/entropy-public-check.XXXXXX")"
MOUNT="$WORK/mount"
mkdir "$MOUNT"
MOUNTED=0
cleanup() {
    local status=$?
    trap - EXIT
    if [[ "$MOUNTED" == 1 ]]; then
        if ! hdiutil detach "$MOUNT" >/dev/null 2>&1; then
            echo 'Failed to detach validation DMG' >&2
            exit 1
        fi
    fi
    rm -rf "$WORK" || exit 1
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
# Set before attach: even a partially successful attach must be cleaned up.
MOUNTED=1
hdiutil attach -readonly -nobrowse -mountpoint "$MOUNT" "${DMGS[0]}" >/dev/null
[[ -d "$MOUNT/Entropy.app" && -L "$MOUNT/Applications" &&
   "$(readlink "$MOUNT/Applications")" == /Applications ]] || exit 1
shopt -s nullglob
VISIBLE=("$MOUNT"/*)
[[ "${#VISIBLE[@]}" == 2 ]] || exit 1
lipo "$MOUNT/Entropy.app/Contents/MacOS/entropy" -verify_arch "$ARCH"
for component in "$MOUNT/Entropy.app/Contents/MacOS/entropy" "$MOUNT/Entropy.app"; do
    macos_verify_stable_signature "$component" "$MACOS_RELEASE_CERTIFICATE_SHA1" com.ergohaven.entropy
done
hdiutil detach "$MOUNT" >/dev/null
MOUNTED=0
# Never upload keychains, importer output, raw security diagnostics, ZIPs or logs.
OUT="$ROOT/dist/signing-test"
rm -rf "$OUT"
mkdir -p "$OUT"
cp "${DMGS[0]}" "$OUT/entropy-signing-test-$ARCH.dmg"
{
    printf 'source_sha=%s\ntarget=%s\ncertificate_sha1=%s\n' \
        "$GITHUB_SHA" "$TARGET" "$MACOS_RELEASE_CERTIFICATE_SHA1"
    printf 'bundle_id=com.ergohaven.entropy\ndesignated_requirement=%s\n' \
        "$(macos_stable_designated_requirement "$MACOS_RELEASE_CERTIFICATE_SHA1" com.ergohaven.entropy)"
    printf '%s\n' 'binary_and_bundle_signature=verified' 'dmg_integrity=verified' \
        'readonly_mounted_dmg=verified' 'applications_symlink=verified' \
        'temporary_keychain_deleted=yes' 'search_list_restored=yes'
} > "$OUT/signing-evidence.txt"
(cd "$OUT" && shasum -a 256 "entropy-signing-test-$ARCH.dmg" signing-evidence.txt > SHA256SUMS)
