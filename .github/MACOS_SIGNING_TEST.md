# Maintainer-only macOS signing validation (PR #195)

This is an **artifact-only test**, not a release. A maintainer with upstream write
access must review the complete PR revision, fetch its **full commit SHA** from
`D19ital/entropy`, and publish that exact commit (no cherry-pick, merge or rebase)
to `ergohaven/entropy:ci/macos-signing-validation-pr195`. Do not update `main`,
create a tag, force-push, or dispatch from the fork.

The already-registered `.github/workflows/release.yml` accepts a manual dispatch
on that exact branch with `macos_signing_test=true`. Dispatches from other refs,
with the default `false`, or in other repositories skip signing. PR events have
no route to these secrets. Checkout uses the immutable dispatch `github.sha`, not
an input ref. Keep the test branch maintainer-controlled and verify the run's
`headSha` equals the reviewed SHA before using its evidence.

Required upstream repository secrets (the existing certificate, no new identity):

- `MACOS_CERTIFICATE_P12_BASE64`: base64 of the encrypted PKCS#12 containing the
  existing pinned certificate and private key.
- `MACOS_CERTIFICATE_PASSWORD`: its export password.

Secret availability cannot be inferred from the fork or a token's 403 response.
Never paste secret values into commands, logs, comments, or artifacts.

## What the dispatch does

Only native macOS ARM64 and Intel jobs run. Cargo and the Swift importer compile
without signing credentials; no shared build cache is restored or saved. Only
the signing step receives the two secrets. It uses the existing pinned signing
helper, disposable keychain, and stable certificate-root/bundle-ID requirement.
Cleanup deletes the temporary keychain, restores and rereads the exact original
search list, and emits fixed public markers only on success. The default
keychain and trust settings are not changed.

A separate secret-free step mounts the actual DMG read-only, checks its app and
Applications symlink, architecture, strict executable/bundle signatures and
exact designated requirements, then detaches it. Each architecture uploads only:

- its signed application DMG (the image itself is not separately codesigned);
- `SHA256SUMS` (DMG and evidence checksums);
- `signing-evidence.txt` (source SHA, target, public certificate pin/requirement,
  verification and cleanup success markers).

Artifacts expire after seven days. Any signing, cleanup or verification failure
prevents upload. No release job can run on `workflow_dispatch`. Existing upstream
`v0.*` tag-push release behavior is retained.

## Operator sequence

Use macOS Terminal with Git and GitHub CLI. Authenticate `gh` as your own upstream
write account (`gh auth login --hostname github.com --git-protocol https --web`),
not Neo's limited token; unset inherited `GH_TOKEN`/`GITHUB_TOKEN` overrides in the
operator shell. Check `gh api user --jq .login` and upstream `.permissions.push`.
Use a fresh scratch clone and a clone-local `gh auth git-credential` helper so
Git and `gh` use that same account. The PR handoff supplies the exact full SHA.
Fetch it directly from the fork, verify `HEAD`, push normally to the dedicated
branch, and read back the upstream ref to confirm the SHA. Stop if that branch
already points at another revision; do not force it.

Then run:

```sh
gh workflow run release.yml --repo ergohaven/entropy \
  --ref ci/macos-signing-validation-pr195 -f macos_signing_test=true
gh run list --repo ergohaven/entropy --workflow release.yml \
  --branch ci/macos-signing-validation-pr195 --event workflow_dispatch \
  --limit 5 --json databaseId,headSha,status,conclusion,url
```

Both architectures must succeed for the **exact reviewed SHA**. Download both
artifacts, verify their `SHA256SUMS`, and inspect their public signature/cleanup
evidence. Secret-free PR CI and mocked tests do **not** prove that the production
PKCS#12 imports successfully. That remains untested until this dispatch runs.
Self-signing is not notarization; Gatekeeper may still require Open Anyway.
Intel permission persistence still needs a separate manual hardware test.
