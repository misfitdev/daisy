default:
    @just --list

# Build the command-line tool
build:
    cargo build

# Run the test suite
test:
    cargo test

# Lint and check formatting
lint:
    cargo clippy --all-targets -- -D warnings
    cargo fmt --check

# Format the code
fmt:
    cargo fmt

# Run daisy with arguments, e.g. `just run listen --pair`
run *args:
    cargo run --quiet -- {{args}}

# Everything CI would check
check: lint test

# Update dependencies and list what is still behind, majors included
update:
    cargo update --verbose
    cd site && npm update
    pinact run -u
    mise outdated --bump
    cd site && npm outdated || true

# macOS files permissions under this ID; changing it means granting them again
bundle_id := "dev.misfit.daisy"
app := "target/Daisy.app"
# DAISY_SIGN_IDENTITY, or the first Apple Development identity
sign_identity := '''${DAISY_SIGN_IDENTITY:-$(security find-identity -v -p codesigning | awk '/"Apple Development/ {print $2; exit}')}'''

# Build Daisy.app and sign it: Apple Development by default, or the
# identity in DAISY_SIGN_IDENTITY ("-" signs ad hoc, as CI does)
bundle:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release
    identity="{{sign_identity}}"
    if [ -z "$identity" ]; then
        echo "no Apple Development signing identity found; see security find-identity -v -p codesigning" >&2
        exit 1
    fi
    version="$(awk -F'"' '/^version = / {print $2; exit}' Cargo.toml)"
    rm -rf "{{app}}"
    mkdir -p "{{app}}/Contents/MacOS" "{{app}}/Contents/Resources"
    sed -e "s/__BUNDLE_ID__/{{bundle_id}}/" -e "s/__VERSION__/$version/" macos/Info.plist > "{{app}}/Contents/Info.plist"
    plutil -lint -s "{{app}}/Contents/Info.plist"
    cp -f target/release/daisy "{{app}}/Contents/MacOS/daisy"
    icon_work="$(mktemp -d)"
    trap 'rm -rf "$icon_work"' EXIT
    mkdir -p "$icon_work/Daisy.iconset"
    sips -s format png macos/DaisyIcon.svg --out "$icon_work/Daisy-1024.png" >/dev/null
    for spec in "icon_16x16.png:16" "icon_16x16@2x.png:32" "icon_32x32.png:32" "icon_32x32@2x.png:64" "icon_128x128.png:128" "icon_128x128@2x.png:256" "icon_256x256.png:256" "icon_256x256@2x.png:512" "icon_512x512.png:512" "icon_512x512@2x.png:1024"; do
        name="${spec%%:*}"
        pixels="${spec##*:}"
        sips -z "$pixels" "$pixels" "$icon_work/Daisy-1024.png" --out "$icon_work/Daisy.iconset/$name" >/dev/null
    done
    iconutil -c icns "$icon_work/Daisy.iconset" -o "{{app}}/Contents/Resources/Daisy.icns"
    # notarization requires a secure timestamp on distributed builds
    timestamp=""
    case "$identity" in "Developer ID"*) timestamp="--timestamp" ;; esac
    # hardened runtime now, so notarizing later changes nothing at run time
    codesign --force --options runtime ${timestamp:+"$timestamp"} --sign "$identity" --identifier "{{bundle_id}}" "{{app}}"
    codesign --verify --strict "{{app}}"
    echo "signed {{app}} with $identity"

# Draw the Daisy window with sample systems for the website
screenshot:
    cargo run --release --quiet -- screenshot site/public/screenshots/daisy-window.png

# Run Daisy.app with arguments, e.g. `just app permissions`
app *args: bundle
    # run from a terminal, the app's binary relaunches itself as the app, so
    # it has its own permissions, and stops when Ctrl-C stops the launcher
    "{{app}}/Contents/MacOS/daisy" {{args}}

dist := "target/dist"

# Zip the signed app and build a DMG for release; both are notarized and
# stapled when NOTARY_KEY_ID, NOTARY_ISSUER_ID and NOTARY_KEY_PATH name an App
# Store Connect API key
package: bundle
    #!/usr/bin/env bash
    set -euo pipefail
    version="$(awk -F'"' '/^version = / {print $2; exit}' Cargo.toml)"
    zip="{{dist}}/Daisy-$version-macos-arm64.zip"
    dmg="{{dist}}/Daisy-$version-macos-arm64.dmg"
    identity="{{sign_identity}}"
    mkdir -p "{{dist}}"
    rm -f "{{dist}}"/Daisy-*
    notarize() {
        result="$(xcrun notarytool submit "$1" --key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" \
            --issuer "$NOTARY_ISSUER_ID" --wait --output-format json)"
        status="$(plutil -extract status raw - <<< "$result")"
        if [ "$status" != "Accepted" ]; then
            echo "notarization of $1 finished as $status:" >&2
            id="$(plutil -extract id raw - <<< "$result")"
            xcrun notarytool log "$id" --key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER_ID" >&2
            exit 1
        fi
    }
    ditto -c -k --keepParent "{{app}}" "$zip"
    if [ -n "${NOTARY_KEY_ID:-}" ]; then
        notarize "$zip"
        # zip again so the download carries the ticket and opens offline
        xcrun stapler staple "{{app}}"
        rm -f "$zip"
        ditto -c -k --keepParent "{{app}}" "$zip"
    fi
    stage="$(mktemp -d)"
    trap 'rm -rf "$stage"' EXIT
    ditto "{{app}}" "$stage/Daisy.app"
    ln -s /Applications "$stage/Applications"
    hdiutil create -quiet -volname Daisy -srcfolder "$stage" -format ULFO -ov "$dmg"
    # ad hoc signatures do not apply to disk images
    if [ "$identity" != "-" ]; then
        timestamp=""
        case "$identity" in "Developer ID"*) timestamp="--timestamp" ;; esac
        codesign --sign "$identity" ${timestamp:+"$timestamp"} "$dmg"
    fi
    if [ -n "${NOTARY_KEY_ID:-}" ]; then
        notarize "$dmg"
        xcrun stapler staple "$dmg"
        xcrun stapler validate "$dmg"
        spctl -a -t open --context context:primary-signature -v "$dmg"
    fi
    for file in "$zip" "$dmg"; do
        (cd "{{dist}}" && shasum -a 256 "$(basename "$file")" > "$(basename "$file").sha256")
    done
    ls -l "{{dist}}"

# Release notes for the current version, from conventional commits
notes:
    @git cliff --latest --strip header 2>/dev/null || git cliff --unreleased --strip header

# Serve the website locally at http://localhost:4321/
site-dev:
    cd site && npm ci && npm run dev

# Build the website into site/dist
site-build:
    cd site && npm ci && npm run build
