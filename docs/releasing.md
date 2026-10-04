# Releasing

For maintainers. Official release artifacts are built, signed, notarized and published by `.github/workflows/release.yml` from a version tag.

## One-time setup

The workflow needs five repository secrets:

| Secret | What it contains |
|---|---|
| `DEVELOPER_ID_P12` | Developer ID Application certificate and private key, as a base64-encoded `.p12` |
| `DEVELOPER_ID_P12_PASSWORD` | Password used when exporting the `.p12` |
| `NOTARY_KEY_ID` | App Store Connect API key ID |
| `NOTARY_ISSUER_ID` | Issuer ID shown with that key |
| `NOTARY_KEY` | Contents of the key's `.p8` file |

Create them:

1. In Xcode, open Settings → Accounts → Manage Certificates → **+** → Developer ID Application. Export it from Keychain Access under My Certificates so the private key is included, as a password-protected `.p12`.
2. In App Store Connect, open Users and Access → Integrations → Team Keys → **+**, with the Developer role. Download the `.p8`; it can only be downloaded once.
3. From a terminal authenticated to GitHub as a repository administrator:

```bash
base64 -i DeveloperID.p12 | gh secret set DEVELOPER_ID_P12 -R misfitdev/daisy
gh secret set DEVELOPER_ID_P12_PASSWORD -R misfitdev/daisy
gh secret set NOTARY_KEY_ID -R misfitdev/daisy
gh secret set NOTARY_ISSUER_ID -R misfitdev/daisy
gh secret set NOTARY_KEY -R misfitdev/daisy < AuthKey_XXXXXXXXXX.p8
```

Delete the local `.p12` and `.p8` after the secrets are stored.

## Cutting a release

1. Set `version` in `Cargo.toml` and make sure `main` is green.
2. Run `just update`. It updates crates, site packages and the pinned GitHub
   Actions, then lists everything still behind, including major versions and
   the tool pins in `.mise.toml`. Take every update, majors included, and
   commit the result.
3. Run the local release review:

   ```bash
   just check
   mise exec -- cargo audit
   mise exec -- cargo deny check
   mise exec -- reachsec check --path .
   mise exec -- zizmor .github
   mise exec -- actionlint
   ```

4. Tag and push, for example `git tag v0.1.0 && git push origin v0.1.0`. The tag must match `Cargo.toml` or the workflow stops.
5. The workflow runs `just check`, builds `Daisy.app`, signs it with Developer ID, notarizes and staples it, and creates the zip and a DMG that is signed, notarized and stapled the same way. It records a GitHub artifact attestation, generates SLSA Build Level 3 provenance through the OpenSSF generator, and publishes a GitHub release with notes since the previous tag.

## Verifying a release

Either command proves that a release file was built by this repository's release workflow:

```bash
gh attestation verify Daisy-0.1.0-macos-arm64.dmg --repo misfitdev/daisy

slsa-verifier verify-artifact Daisy-0.1.0-macos-arm64.dmg \
  --provenance-path Daisy-0.1.0-macos-arm64.dmg.intoto.jsonl \
  --source-uri github.com/misfitdev/daisy --source-tag v0.1.0
```

To verify Apple's notarization of the DMG, and of the app inside it or the unzipped app:

```bash
spctl --assess --type open --context context:primary-signature -vv Daisy-0.1.0-macos-arm64.dmg
spctl --assess --type execute -vv Daisy.app
```

Both should report `source=Notarized Developer ID`.

## Release notes

Release notes come from Conventional Commit subjects through `git cliff`; `just notes` previews them. `feat`, `fix`, `perf` and `docs` commits are listed. `chore`, `ci`, `build`, `test`, `style` and `refactor` commits are omitted.
