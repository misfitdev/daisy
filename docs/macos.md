# macOS internals

Implementation reference for input capture and replay. To grant or repair permissions, see the [User guide](usage.md#permissions).

What Daisy relies on in macOS, including undocumented parts, and what was measured to establish it. Hardware observations below name the relevant macOS versions; they were not all established by one machine pair.

## Device signing

`src/macos/device.rs` creates non-exportable P-256 Secure Enclave keys in the Data Protection Keychain. The private key uses `AfterFirstUnlockThisDeviceOnly` protection with private-key usage and no user-presence requirement. Daisy stores only a randomly generated Keychain label in `device-identity`, resolves that label within the signed app’s access group, and checks that the returned key belongs to the Secure Enclave. Signatures use ECDSA with SHA-256 and DER encoding; public keys use compressed SEC1 encoding.

The signed app must embed a provisioning profile authorizing its application identifier and certificate. Key creation and rotation require an unlocked system; signing with an existing key works after the first unlock. Rotation replaces the saved reference, retires the old key, and restores the prior reference if retirement fails.

## Permissions

Reading input needs Input Monitoring; posting input needs Accessibility. macOS grants both to a signed app, keyed to its bundle ID and signing certificate rather than a hash, so grants survive rebuilds and updates signed the same way.

macOS judges a process by the app responsible for it. A binary run directly from Terminal, even inside `Daisy.app`, borrows Terminal's permissions and hides the permission bugs that matter. Daisy therefore relaunches itself through `open`, making the app responsible for itself, and waits. Ctrl-C stops the launcher; the app notices within 250 ms and shuts down cleanly.

The Daisy setup window reports each grant and has a **Set Up…** action for anything missing. The optional `daisy permissions --request` command provides the same check and prompt path.

## The event tap

Each system taps its own input at the HID level before applications see it. It requests every event type and filters them itself because requesting only the private gesture types delivers nothing.

The callback runs for every input event on the system. It must never wait, perform I/O, start a process or panic into CoreGraphics. State acquisition and queue sends are nonblocking. Contention or queue overload passes the event through locally, reclaims local routing where possible and asks the runtime to end the session.

## Pinning the pointer

While the peer has control, the local pointer should stay still. On macOS 27, `CGAssociateMouseAndMouseCursorPosition(false)` reports success but is ignored.

Daisy therefore hides the pointer and warps it back to the crossing point whenever it drifts more than 1.5 points. Post-warp input delay is disabled. A background app may change the cursor only after setting the private `SetsCursorInBackground` WindowServer connection property.

## Clicks

macOS 27 ignores synthetic clicks and drags without an event number. The system replaying input numbers them starting just above the event count macOS keeps. Click count comes from the system sending input, so double-click timing follows its settings.

## Display activity

While a group has connected members, `PreventUserIdleSystemSleep` holds off
idle system sleep. It does not keep displays awake permanently.

New physical input on the system in control sends a generation-stamped
`Activity` message to every connected member, at most once per second. An
unlocked member accepts it only from the current owner while no local input
is taking precedence. The injector refreshes `IOPMAssertionDeclareUserActivity`
and retains the returned assertion ID for the next refresh. IOKit expires it
using the user's display sleep timeout; the injector releases it when the
session ends. Heartbeats and clipboard traffic do not refresh this assertion.
No input event is synthesized to report display activity, and locked systems
remain locked.

## Swipes

Trackpad swipes that switch Spaces or open Mission Control arrive as private `DockControl` events (type 30), each followed by a companion gesture event (type 29), not as public gesture events. The field layout is confined to `src/macos/swipe.rs`.

Native captures on macOS 26.6.1 (2026-10-05) use the same DockControl subtype (23), horizontal/vertical motion values (1/2), phases (1/2/4/8), progress and velocity fields as the macOS 27 decoder. Swipe capture is enabled on both versions.

The system in use forwards each swipe step: beginning, progress, ending velocity or cancellation. A swipe stays with the system that had control when it began.

- **macOS 27 replay:** the peer replays a live synthetic swipe, so progress follows the fingers and can pause, reverse or cancel. Each step carries a serialized raw IOHID queue element in CGEvent field 4205. Dock ignores steps posted back to back, so a poster task spaces them by at least 16 ms and coalesces stale progress. A swipe still underway when control leaves or the session ends is cancelled.

Real-trackpad end-to-end verification covered live replay, including progress, pullback, cancellation and completion.

On synthetic input, Dock decides by the direction of the last movement rather than distance: a held swipe released anywhere completes, while one pulled back stays. The ignored hardware test `the_dock_follows_a_replayed_swipe` exercises this on a system with a Space to the right.

Direction signs were measured on hardware:

| Gesture | Real swipe reports | Synthetic swipe needs |
|---|---:|---:|
| Left Space | negative | positive |
| Up, Mission Control | negative | positive |

Real and synthetic signs are opposite on both axes. Tests pin recorded values because replaying a capture with its own sign would let the same mistake cancel out.

## Shake to locate

macOS enlarges the pointer when physical input shakes it, but the system detector ignores posted motion. This was confirmed on macOS 27 by sampling `CGSGetCursorScale` through 200 synthetic shake events: the scale remained 1.0.

The system replaying input therefore detects shake in replayed motion through `src/shake.rs`: five quick reversals within one second, each stroke at least 80 points and under 200 ms, horizontally or vertically. It grows the pointer toward four times its configured size through private `CGSSetCursorScale`, then shrinks it within about one third of a second.

The behavior follows that system's “Shake mouse pointer to locate” setting and Accessibility pointer size. Pointer scale outlives the process that changed it, so Daisy restores the configured size when magnification ends, when the session ends and whenever any Daisy command starts.
