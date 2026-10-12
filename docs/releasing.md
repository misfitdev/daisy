# Releasing

Daisy's build and test jobs use `macos-26`; Pages and release support jobs use
`ubuntu-24.04`. Runner upgrades are explicit workflow changes. The SLSA generator
selects its own runners through its pinned upstream reusable workflow.

Build and publish an official release. For a local development build, start with [Contributing](../CONTRIBUTING.md); for installing an existing release, see the [Getting started](getting-started.md).

For maintainers. Official release artifacts are built, signed, notarized and published by `.github/workflows/release.yml` from a version tag.

Signed app builds require a macOS provisioning profile for `dev.misfit.daisy` that authorizes the selected signing certificate. Set `DAISY_PROVISIONING_PROFILE` to its local path. The bundle recipe validates the profile, embeds it, and derives the application identifier and team entitlements for Daisy’s default Keychain access group. The default signing identity is Developer ID Application; `DAISY_SIGN_IDENTITY` can select another certificate authorized by the profile.

Release CI reads the profile from the `DEVELOPER_ID_PROFILE_BASE64` repository secret. Ad hoc CI bundles exercise build and interface checks; they cannot create a Keychain device identity.

## One-time setup

The workflow needs these repository secrets (or org-level secrets scoped to
this repository):

| Secret | What it contains |
|---|---|
| `DEVELOPER_ID_P12` | Developer ID Application certificate and private key, as a base64-encoded `.p12` |
| `DEVELOPER_ID_PROFILE_BASE64` | Base64-encoded Developer ID provisioning profile for `dev.misfit.daisy`, authorizing the signing certificate |
| `DEVELOPER_ID_P12_PASSWORD` | Password used when exporting the `.p12` |
| `NOTARY_KEY_ID` | App Store Connect API key ID |
| `NOTARY_ISSUER_ID` | Issuer ID shown with that key |
| `NOTARY_KEY` | Contents of the key's `.p8` file |
| `SPARKLE_PRIVATE_KEY` | Base64 Ed25519 update signing key matching `macos/sparkle-public-key.txt` |
| `HOMEBREW_TAP_TOKEN` | Fine-grained GitHub PAT, scoped to Contents: write on `misfitdev/homebrew-tap` only |

Create them:

1. In Xcode, open Settings → Accounts → Manage Certificates → **+** → Developer ID Application. Export it from Keychain Access under My Certificates so the private key is included, as a password-protected `.p12`.
2. In Apple Developer Certificates, Identifiers & Profiles, create a Developer ID provisioning profile for `dev.misfit.daisy` and the signing certificate. Daisy uses the app’s default Keychain access group; no separate Keychain Sharing capability or user-presence requirement is needed. Download the profile.
3. In App Store Connect, open Users and Access → Integrations → Team Keys → **+**, with the Developer role. Download the `.p8`; it can only be downloaded once.
4. At github.com/settings/personal-access-tokens, create a fine-grained PAT scoped to the `misfitdev/homebrew-tap` repository only, with Contents: write permission.
5. From a terminal authenticated to GitHub as a repository administrator:

```bash
base64 -i DeveloperID.p12 | gh secret set DEVELOPER_ID_P12 -R misfitdev/daisy
base64 -i Daisy.provisionprofile | gh secret set DEVELOPER_ID_PROFILE_BASE64 -R misfitdev/daisy
gh secret set DEVELOPER_ID_P12_PASSWORD -R misfitdev/daisy
gh secret set NOTARY_KEY_ID -R misfitdev/daisy
gh secret set NOTARY_ISSUER_ID -R misfitdev/daisy
gh secret set NOTARY_KEY -R misfitdev/daisy < AuthKey_XXXXXXXXXX.p8
gh secret set HOMEBREW_TAP_TOKEN -R misfitdev/daisy
```

Configure `SPARKLE_PRIVATE_KEY` using the export steps in
[Update feed and framework](#update-feed-and-framework).

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

4. Tag and push the version recorded in `Cargo.toml`:

   ```bash
   version="$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["package"]["version"])')"
   git tag "v$version"
   git push origin "v$version"
   ```
5. The workflow runs `just check`, builds `Daisy.app`, signs it with Developer ID, notarizes and staples it, and creates the zip and a DMG that is signed, notarized and stapled the same way. It records a GitHub artifact attestation, generates SLSA Build Level 3 provenance through the OpenSSF generator, and publishes a GitHub release with notes since the previous tag.

## Verifying a release

The bundle recipe embeds `DaisyNetworkProtocol` in the signed `Info.plist`,
using the compiled binary's `update-info` command. That command exits before
identity, input capture or listener initialization. Feed generation uses this compiled metadata.

Packaging also emits `Daisy-<version>-update.toml` from the release source.
It records metadata format `2`, package version, `session::PROTOCOL`, the
supported protocol list, and the final ZIP SHA-256. The release workflow
attests the file and includes it in SLSA provenance. Feed generation checks this metadata against the final ZIP digest.
The native updater uses signed feed channels for compatibility.

Either command proves that a release file was built by this repository's release workflow:

```bash
release_tag="$(gh release view --repo misfitdev/daisy --json tagName --jq .tagName)"
artifact="Daisy-${release_tag#v}-macos-arm64.dmg"
gh release download "$release_tag" --repo misfitdev/daisy \
  --pattern "$artifact" --pattern multiple.intoto.jsonl

gh attestation verify "$artifact" --repo misfitdev/daisy

slsa-verifier verify-artifact "$artifact" \
  --provenance-path multiple.intoto.jsonl \
  --source-uri github.com/misfitdev/daisy --source-tag "$release_tag"
```

To verify Apple's notarization of the DMG, and of the app inside it or the unzipped app:

```bash
spctl --assess --type open --context context:primary-signature -vv "$artifact"
spctl --assess --type execute -vv Daisy.app
```

Both should report `source=Notarized Developer ID`.

## Homebrew cask

Its version and SHA-256 come from the final signed and notarized DMG; never
substitute an unsigned build's hash. The cask requires Apple silicon and
macOS 26 or later, installs `Daisy.app`, and leaves device identity, peer
trust, and settings intact when uninstalled.

The release workflow generates and publishes the cask itself: the
`update-homebrew` job pushes `Casks/daisy.rb` to
[`misfitdev/homebrew-tap`](https://github.com/misfitdev/homebrew-tap) after
every release, authenticating with the `HOMEBREW_TAP_TOKEN` repository
secret (a fine-grained PAT scoped to Contents: write on that repo only).
Users install and upgrade with:

```bash
brew install --cask misfitdev/tap/daisy
brew update
brew upgrade --cask misfitdev/tap/daisy
```

To generate the cask locally, e.g. to check its contents before a release:

```bash
version="$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["package"]["version"])')"
python3 tools/homebrew-cask.py "target/dist/Daisy-$version-macos-arm64.dmg" target/dist/daisy.rb
```

Homebrew's livecheck still watches for new stable releases for its own
metadata, but `update-homebrew` is what actually advances the pinned version
and SHA-256 in the tap.
Advanced settings offers three update policies: ask before installing,
auto-install routine updates, or auto-install every update. Routine updates
retain the same major version and every protocol supported by the current
build. The signed feed authenticates that selection. Automatic checks and
downloads stop when **Check for updates automatically** is off; an update
already downloaded and accepted can still install on quit.

Accessibility and Input Monitoring still require approval in System Settings.
The cask is release metadata; the DMG and ZIP are the assets covered by the
existing provenance and artifact attestations.

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
runs on the default branch with the selected release tag.

To redeploy the published release selected in [Verifying a release](#verifying-a-release):

```bash
gh workflow run pages.yml --ref main -f "release_tag=$release_tag"
```

Omit `release_tag` to use the latest stable release. Pages dispatch and
deployment are separate runs; the release workflow reports the dispatch,
and the Pages workflow reports its test, build, and deployment results.

## Release notes

Release notes come from Conventional Commit subjects through `git cliff`; `just notes` previews them. `feat`, `fix`, `perf` and `docs` commits are listed. `chore`, `ci`, `build`, `test`, `style` and `refactor` commits are omitted.

## Update feed and framework

`just sparkle` downloads the official native updater framework at the revision
and SHA-256 pinned in `tools/sparkle.py`. The Rust wrapper links this framework;
packaging copies it into `Daisy.app/Contents/Frameworks` with its symlinks intact.
Nested XPC services, the installer executable, updater application and framework
are signed before the outer application. Ad hoc development builds disable
library validation; distributed builds retain it.

The public update key is committed in `macos/sparkle-public-key.txt`. The
matching private key is stored locally under the Keychain account
`dev.misfit.daisy.sparkle`. To export it directly to the release secret without
printing the private key:

```bash
key_dir="$(mktemp -d)"
key_file="$key_dir/private-key"
umask 077
target/sparkle/bin/generate_keys --account dev.misfit.daisy.sparkle -x "$key_file"
gh secret set SPARKLE_PRIVATE_KEY < "$key_file"
rm -f "$key_file"
rmdir "$key_dir"
```

Back up that private key securely. Changing the embedded public key without a
key-rotation migration prevents installed releases from accepting updates.

The release workflow writes the key to a private temporary file, exposes its
path as `DAISY_SPARKLE_KEY_FILE`, and removes it when packaging exits. Local
packaging uses that variable when supplied, otherwise the named Keychain key.
Ad hoc CI packages contain the framework but do not generate production feeds.

`tools/sparkle_appcast.py` verifies the preceding feed's signature, retains its
older entries, adds the current release in stable and major/protocol channels,
signs the final ZIP, and signs the feed. The resulting `appcast.xml` is uploaded
with the release assets and included in artifact attestations and SLSA subjects.
The release workflow dispatches Pages with its explicit tag; Pages downloads
that release's feed to `site/public/updates/appcast.xml` before building. PR site
builds do not publish feeds. A first release can start a feed when its URL returns
404; other fetch or verification failures stop generation.

For an isolated native check with disposable bundles and signing keys:

```bash
just sparkle
cargo build --example sparkle_smoke
python3 tools/test_sparkle_native.py --identity developer-id
```

This checks automatic installation on quit, reopening the resulting bundle,
native install-and-relaunch with the selected data directory and display name,
retry after an injected relaunch-state write failure,
code-signature validity, and rejection of modified feeds and archives. It does
not replace the installed application or use the production update signing key.
