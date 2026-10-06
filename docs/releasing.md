# Releasing

Build and publish an official release. For a local development build, start with [Contributing](../CONTRIBUTING.md); for installing an existing release, see the [Getting started](getting-started.md).

For maintainers. Official release artifacts are built, signed, notarized and published by `.github/workflows/release.yml` from a version tag.

Signed app builds require a macOS provisioning profile for `dev.misfit.daisy` that authorizes the selected signing certificate. Set `DAISY_PROVISIONING_PROFILE` to its local path. The bundle recipe validates the profile, embeds it, and derives the application identifier and team entitlements for Daisy’s default Keychain access group. The default signing identity is Developer ID Application; `DAISY_SIGN_IDENTITY` can select another certificate authorized by the profile.

Release CI reads the profile from the `DEVELOPER_ID_PROFILE_BASE64` repository secret. Ad hoc CI bundles exercise build and interface checks; they cannot create a Keychain device identity.

## One-time setup

The workflow needs six repository secrets:

| Secret | What it contains |
|---|---|
| `DEVELOPER_ID_P12` | Developer ID Application certificate and private key, as a base64-encoded `.p12` |
| `DEVELOPER_ID_PROFILE_BASE64` | Base64-encoded Developer ID provisioning profile for `dev.misfit.daisy`, authorizing the signing certificate |
| `DEVELOPER_ID_P12_PASSWORD` | Password used when exporting the `.p12` |
| `NOTARY_KEY_ID` | App Store Connect API key ID |
| `NOTARY_ISSUER_ID` | Issuer ID shown with that key |
| `NOTARY_KEY` | Contents of the key's `.p8` file |

Create them:

1. In Xcode, open Settings → Accounts → Manage Certificates → **+** → Developer ID Application. Export it from Keychain Access under My Certificates so the private key is included, as a password-protected `.p12`.
2. In Apple Developer Certificates, Identifiers & Profiles, create a Developer ID provisioning profile for `dev.misfit.daisy` and the signing certificate. Daisy uses the app’s default Keychain access group; no separate Keychain Sharing capability or user-presence requirement is needed. Download the profile.
3. In App Store Connect, open Users and Access → Integrations → Team Keys → **+**, with the Developer role. Download the `.p8`; it can only be downloaded once.
4. From a terminal authenticated to GitHub as a repository administrator:

```bash
base64 -i DeveloperID.p12 | gh secret set DEVELOPER_ID_P12 -R misfitdev/daisy
base64 -i Daisy.provisionprofile | gh secret set DEVELOPER_ID_PROFILE_BASE64 -R misfitdev/daisy
gh secret set DEVELOPER_ID_P12_PASSWORD -R misfitdev/daisy
gh secret set NOTARY_KEY_ID -R misfitdev/daisy
gh secret set NOTARY_ISSUER_ID -R misfitdev/daisy
gh secret set NOTARY_KEY -R misfitdev/daisy < AuthKey_XXXXXXXXXX.p8
```

Delete the local `.p12` and `.p8` after the secrets are stored.

## Cutting a release

Keep the stable session protocol for compatible releases; a release-version
bump does not require a protocol bump. Follow the
[protocol compatibility rules](protocol.md#changing-the-protocol) before
changing any wire type or peer behavior. A protocol break requires a manual
group update and must be explained in the release notes.

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

The bundle recipe embeds `DaisyNetworkProtocol` in the signed `Info.plist`,
using the compiled binary's `update-info` command. That command exits before
identity, input capture or listener initialization. The local updater reads
this sealed value and the bundle version instead of executing the replacement
while the current app is sharing.

Packaging also emits `Daisy-<version>-update.toml` from the release source. It
records metadata format `1`, the package version, `session::PROTOCOL`, and the
SHA-256 of the final ZIP. The release workflow covers this file with artifact
attestation and SLSA provenance. It is the compatibility contract for the
future automatic installer; release numbers and release-note text are not a
substitute. Verify its provenance and archive binding before using it.

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

## Homebrew cask

The release workflow generates `daisy.rb` from the final signed and notarized
DMG and includes it in the release assets. Its version and SHA-256 come from
that DMG; never substitute an unsigned build's hash. The cask requires Apple
silicon and macOS 26 or later, installs `Daisy.app`, and leaves device identity,
peer trust, and settings intact when uninstalled.

To generate it locally after packaging:

```bash
python3 tools/homebrew-cask.py target/dist/Daisy-0.6.0-macos-arm64.dmg target/dist/daisy.rb
```

Publish the generated file as `Casks/daisy.rb` in a Homebrew tap. For a tap
named `misfitdev/homebrew-daisy`, users install and upgrade with:

```bash
brew install --cask misfitdev/daisy/daisy
brew update
brew upgrade --cask misfitdev/daisy/daisy
```

These commands require the tap to be published. After each stable release,
update its cask using that release's generated `daisy.rb`. Homebrew's livecheck
detects new stable releases, but does not update the tap's recipe itself.
Daisy does not install updates automatically. Quit Daisy before upgrading,
then reopen it; install the same compatible version on every group member.
Accessibility and Input Monitoring still require approval in System Settings.
Users can also install the release's recipe through a local tap; see
[Homebrew installation](usage.md#homebrew). The generated recipe is release
metadata; the DMG and ZIP are the assets covered by the existing provenance
and artifact attestations.

## Website CI and deployment

Website and documentation pull requests run the Pages workflow on Linux:
`npm ci`, `npm test`, and `npm run build` in `site/`. The build checks generated documentation routes, local links, images and section anchors; a broken reference fails the build. Pull requests do not
upload a Pages artifact or deploy. Changes limited to the site, documentation,
or task metadata skip the macOS app workflow.

Site and documentation changes on `main` run the same checks and deploy using
the latest published stable release for download links. PR builds use the
version in `Cargo.toml` without querying the release API.

After the release workflow publishes its assets, it dispatches Pages on the
default branch with the exact `release_tag`. Pages validates that release is
published, stable, and contains its versioned DMG before building. Deployment
runs in its own workflow rather than the tag run, avoiding the stale artifact
behavior seen when the same commit was already deployed from `main`.

To redeploy a specific published release:

```bash
gh workflow run pages.yml --ref main -f release_tag=v0.6.0
```

Omit `release_tag` to use the latest stable release. Pages dispatch and
deployment are separate runs; the release workflow reports the dispatch,
and the Pages workflow reports its test, build, and deployment results.

## Release notes

Release notes come from Conventional Commit subjects through `git cliff`; `just notes` previews them. `feat`, `fix`, `perf` and `docs` commits are listed. `chore`, `ci`, `build`, `test`, `style` and `refactor` commits are omitted.
