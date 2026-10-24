default:
    @just --list

# Fetch the pinned native update framework
sparkle:
    python3 tools/sparkle.py fetch

# Build the command-line tool
build: sparkle
    cargo build

# Remove generated builds, including Daisy.app and website output
clean:
    cargo clean --target-dir target
    rm -rf site/dist site/.astro

# Run the test suite
test: sparkle
    cargo test
    python3 tools/test_bundle_signing.py
    python3 tools/test_sparkle_appcast.py
    python3 tools/test_sparkle_native_checks.py
    python3 tools/test_sparkle_fetch.py
    python3 tools/test_trace_summary.py

# Lint and check formatting
lint: sparkle
    # separate target dir: clippy's rustc flags differ from plain build/test,
    # so sharing target/debug invalidates its incremental cache every run
    cargo clippy --target-dir target/clippy --all-targets -- -D warnings
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
# DAISY_SIGN_IDENTITY; otherwise ad hoc without DAISY_PROVISIONING_PROFILE,
# or the first Developer ID Application identity with it
sign_identity := '''${DAISY_SIGN_IDENTITY:-$([ -z "${DAISY_PROVISIONING_PROFILE:-}" ] && echo - || security find-identity -v -p codesigning | awk '/"Developer ID Application/ {print $2; exit}')}'''

# Build Daisy.app and sign it with DAISY_SIGN_IDENTITY, or ad hoc ("-", as CI
# does) when DAISY_PROVISIONING_PROFILE is unset. Ad hoc builds cannot create
# a persistent device identity.
bundle: sparkle
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release
    identity="{{sign_identity}}"
    if [ -z "$identity" ]; then
        echo "no Developer ID Application signing identity found; see security find-identity -v -p codesigning" >&2
        exit 1
    fi
    version="$(awk -F'"' '/^version = / {print $2; exit}' Cargo.toml)"
    metadata="$(target/release/daisy update-info)"
    protocol="$(printf '%s\n' "$metadata" | awk '/^protocol = / {print $3}')"
    protocols="$(printf '%s\n' "$metadata" | sed -n 's/^supported_protocols = \[\(.*\)\]$/\1/p' | tr -d ' ')"
    [[ "$protocol" =~ ^[0-9]+$ && "$protocols" =~ ^([0-9]+,)*[0-9]+$ ]] || { echo "the build did not report supported network protocols" >&2; exit 1; }
    case ",$protocols," in *",$protocol,"*) ;; *) echo "the primary protocol is missing from the supported protocol list" >&2; exit 1 ;; esac
    rm -rf "{{app}}"
    mkdir -p "{{app}}/Contents/MacOS" "{{app}}/Contents/Resources"
    sed -e "s/__BUNDLE_ID__/{{bundle_id}}/" -e "s/__VERSION__/$version/" -e "s/__NETWORK_PROTOCOL__/$protocol/" -e "s/__SUPPORTED_PROTOCOLS__/$protocols/" macos/Info.plist > "{{app}}/Contents/Info.plist"
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
    python3 tools/sparkle.py bundle --app "{{app}}"
    python3 tools/sparkle.py sign --app "{{app}}" --identity "$identity"

    # notarization requires a secure timestamp on distributed builds
    timestamp=""
    [ "$identity" = "-" ] || timestamp="--timestamp"
    # hardened runtime now, so notarizing later changes nothing at run time
    sign_args=(--force --options runtime)
    if [ "$identity" = "-" ]; then
        sign_args+=(--entitlements macos/dev-entitlements.plist)
    fi
    if [ "$identity" != "-" ]; then
        if [ -z "${DAISY_PROVISIONING_PROFILE:-}" ]; then
            echo "set DAISY_PROVISIONING_PROFILE to a macOS profile authorizing this signing certificate and {{bundle_id}}" >&2
            exit 1
        fi
        python3 macos/prepare-profile.py "$DAISY_PROVISIONING_PROFILE" "{{bundle_id}}" "$identity" "{{app}}" "$icon_work/entitlements.plist"
        sign_args+=(--entitlements "$icon_work/entitlements.plist")
    fi
    codesign "${sign_args[@]}" ${timestamp:+"$timestamp"} --sign "$identity" --identifier "{{bundle_id}}" "{{app}}"
    codesign --verify --deep --strict "{{app}}"
    echo "signed {{app}} with $identity"

# Draw the Daisy window with sample systems for the website
screenshot:
    cargo run --release --quiet -- screenshot site/public/screenshots/daisy-window.png

# Run Daisy.app with arguments, e.g. `just app permissions`
app *args: bundle
    # run from a terminal, the app's binary relaunches itself as the app, so
    # it has its own permissions, and stops when Ctrl-C stops the launcher
    "{{app}}/Contents/MacOS/daisy" {{args}}

# Collect developer traces from the installed app in the background until `just trace-stop`
trace output="$HOME/daisy-trace.ndjson":
    #!/usr/bin/env bash
    set -euo pipefail
    pidfile="{{trace_pid}}"
    {{collector}}
    if pid="$(collector_pid)"; then
        echo "a trace collector is already running (pid $pid); run 'just trace-stop' first" >&2
        exit 1
    fi
    rm -f "{{trace_pid}}"
    if [ -e "{{output}}" ]; then
        mv -f "{{output}}" "{{output}}.$(date +%Y%m%d-%H%M%S)"
    fi
    nohup "{{installed}}/Contents/MacOS/daisy" trace --output "{{output}}" </dev/null >"{{trace_log}}" 2>&1 &
    pid=$!
    printf '%s\n%s\n' "$pid" "$(started_at "$pid")" > "{{trace_pid}}"
    sleep 2
    if ! collector_pid >/dev/null; then
        cat "{{trace_log}}" >&2
        rm -f "{{trace_pid}}"
        exit 1
    fi
    echo "collecting into {{output}} (pid $pid)"

# Stop the background trace collector started by `just trace`
trace-stop:
    #!/usr/bin/env bash
    set -euo pipefail
    pidfile="{{trace_pid}}"
    {{collector}}
    if pid="$(collector_pid)"; then
        kill -INT "$pid"
    else
        echo "no trace collector is running" >&2
    fi
    rm -f "{{trace_pid}}"

# Summarize a trace: what was recorded, and every pointer decision in order
trace-summary file=(env('HOME') / 'daisy-trace.ndjson'):
    python3 tools/trace_summary.py {{quote(file)}}

installed := "/Applications/Daisy.app"
trace_pid := "$HOME/Library/Application Support/daisy/trace-collector.pid"
trace_log := "$HOME/Library/Application Support/daisy/trace-collector.log"
# The recorded pid is the collector only while that process still has the
# start time recorded with it; a pid reused by another process never matches.
collector := '''started_at() {
        ps -p "$1" -o lstart= 2>/dev/null | xargs
    }
    collector_pid() {
        [ -f "$pidfile" ] || return 1
        local pid started
        { read -r pid; read -r started; } < "$pidfile" || return 1
        [ -n "$pid" ] && [ "$(started_at "$pid")" = "$started" ] || return 1
        ps -p "$pid" -o command= | grep -q "daisy trace" || return 1
        echo "$pid"
    }'''

# Run an ad hoc Daisy.app on throwaway data with sample peers, to try the
# interface without permissions, pairing or this system's real peers
dev:
    #!/usr/bin/env bash
    set -euo pipefail
    DAISY_SIGN_IDENTITY=- {{just_executable()}} bundle
    home="$(mktemp -d)"
    mkdir -p "$home/trust-v5"
    now="$(date +%s)"
    studio="$(printf '11%.0s' {1..32})"
    cat > "$home/trust-v5/peers.toml" <<EOF
    [[peer]]
    key = "$studio"
    name = "Studio"
    trust = "idle"
    paired_at = $((now - 86400))
    last_seen = $((now - 3600))
    side = "Right"
    side_chosen = 0

    [[peer]]
    key = "$(printf '22%.0s' {1..32})"
    name = "Laptop"
    trust = "30d"
    paired_at = $((now - 86400))
    last_seen = $now
    side = "Left"
    side_chosen = 0

    [[peer]]
    key = "$(printf '33%.0s' {1..32})"
    name = "Desk"
    trust = "forever"
    paired_at = $now
    last_seen = $now
    side = "Left"
    side_chosen = 0
    introduced_by = "$studio"
    EOF
    local_key="$(printf '00%.0s' {1..32})"
    laptop="$(printf '22%.0s' {1..32})"
    offline="$(printf '33%.0s' {1..32})"
    cat > "$home/trust-v5/arrangement.toml" <<EOF
    version = 1
    author = "$local_key"
    [[member]]
    key = "$local_key"
    x = 0.0
    y = 0.0
    [[member]]
    key = "$studio"
    x = 1512.0
    y = 0.0
    [[member]]
    key = "$laptop"
    x = -1512.0
    y = 0.0
    [[member]]
    key = "$offline"
    x = 5000.0
    y = 0.0
    EOF
    echo "sample data in $home; drag Desk into reach or choose Looks Right" >&2
    "{{app}}/Contents/MacOS/daisy" --home "$home" --name "Local system" preview-arrangement

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
    rm -f "{{dist}}"/Daisy-* "{{dist}}"/appcast.xml
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
        [ "$identity" = "-" ] || timestamp="--timestamp"
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
    cargo run --release --quiet --example update_manifest -- "$zip" > "{{dist}}/Daisy-$version-update.toml"
    if [ "$identity" != "-" ]; then
        {{just_executable()}} notes > "{{dist}}/Daisy-$version-notes.md"
        python3 tools/sparkle_appcast.py "$zip" "{{dist}}/Daisy-$version-update.toml" "{{dist}}/Daisy-$version-notes.md" "{{dist}}/appcast.xml"
    fi
    ls -l "{{dist}}"

# Release notes for the tagged commit, or for unreleased commits, from conventional commits
notes:
    @git cliff --current --strip header 2>/dev/null || git cliff --unreleased --strip header

# Serve the website locally at http://localhost:4321/
site-dev:
    cd site && npm ci && npm run dev

# Build the website into site/dist
site-build:
    cd site && npm ci && npm run build
