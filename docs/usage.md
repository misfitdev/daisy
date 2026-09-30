# Usage

Daisy shares one system's keyboard, mouse and supported trackpad gestures with a peer on the same network. It does not use an account or cloud service.

## Requirements

- Apple silicon systems running macOS 26 or later.
- Accessibility and Input Monitoring granted to the signed `Daisy.app` on both systems.
- TCP reachability between the systems. Daisy listens on port 24850 by default.
- macOS 27 on the system whose trackpad swipes should cross. Keyboard and mouse sharing works on macOS 26.

Install the release in `/Applications` and open `Daisy.app`. Daisy lives in the menu bar. It opens the setup window automatically the first time; afterward, choose **Open Daisy…** from the menu-bar flower.

## Pair two systems

On both systems:
1. Choose the screen edge where the peer sits.
2. Choose a trust policy, or leave **Until 4 days inactive**.
3. Click **Pair a New Peer**.

Nearby systems find each other automatically. The connection opener is chosen deterministically; you do not choose a network or input role. One system shows a six-digit code. Enter it on the other. The code is used directly as SPAKE2 input; do not compare codes by eye.

Pairing closes after the first successful unknown peer and is limited to five unknown-key attempts in ten minutes. For a network without Bonjour, enter the peer's local name or IP address on one system and leave the address empty on the other. The **Nearby** menu can also select a particular peer.

## Start and stop sharing

Click **Start Sharing** on both systems, or choose **Start** from the menu-bar flower. Nearby paired peers advertise and find each other automatically, repeating the encrypted handshake and checking trust after a disconnect. The lower public key opens the connection; the other listens. Both can supply input regardless of which opens the connection.

An optional address connects directly and is useful when Bonjour cannot reach the peer. When a paired peer is selected from **Nearby**, Daisy checks its pinned key and discovers its current address on reconnect. Direct connections back off after an established session drops; the automatic nearby workflow continues looking for a paired peer.

Choose **Stop** to end sharing. Trust revocation also ends the session.

## Move control

Push the pointer through the configured shared edge to use the peer. Push it back through the corresponding edge to return. Using either system's own keyboard or trackpad immediately takes control there and releases remote held input; momentum scrolling after a trackpad flick does not. A 150 ms settle window limits repeated claims during simultaneous use; local input always stays local during that window. The screen arrangement stays the same when control changes.

Control never crosses while a mouse button is held. Control-Option-Command-Escape immediately returns control to the system you are at. Disconnect, three seconds of silence, trust revocation or local queue overload also end remote control and release held input.

The menu-bar flower uses color only for state:

- Yellow center: connected.
- Gray center: stopped, looking for a peer, or connecting. The menu text gives the exact state.

The first line of the menu and the top of the Daisy window name the peer in every state: looking for it, connecting, reconnecting or connected. While connected, the menu adds the average round trip to the peer, for example "Connected to Studio · 4 ms", and the window also says which system has control and how long the session has run. The round trip is measured from the heartbeat each system sends every second, averaged over the last five; a single round trip over 50 ms is written to the log.

## Clipboard

When control moves, the clipboard goes with it: the system giving up control sends its clipboard, including when control moves because someone started using the other system. Copy on one system, move to the peer and paste there; copy on the peer, come back (through the edge, by using this system, or with Control-Option-Command-Escape) and paste here.

- Plain text, rich text and images are shared. Images copied as TIFF, such as screenshots, arrive as PNG.
- Text and rich text are limited to 4 MB each and images to 32 MB. Anything larger stays on the system it was copied on; the rest of the copy still crosses.
- Items marked concealed or transient by their source app stay on the system where they were copied.
- Nothing is sent while control stays on one system, and a copy that has already crossed is not sent again.
- A large image is sent behind input, so the pointer never waits for it.

Turn it off with **Share Clipboard** in the menu-bar flower. A system with it off neither sends its clipboard nor accepts one. From the command line, add `--no-clipboard` to `listen` or `connect`.

## Discovery

Each system advertises itself on the local network with Bonjour. The advertisement uses a random name and carries neither the system's name nor its key: only a paired peer can recognise it, and a new advertisement cannot be linked to the last. To stop advertising, turn off **Discoverable on This Network** in the menu-bar flower, or add `--no-discovery` to `listen`. Connecting by name or address keeps working either way, including across networks Bonjour does not reach.

## Permissions

The setup window reports Accessibility and Input Monitoring separately. Choose the corresponding **Grant…** button when either is missing, then finish the change in **System Settings → Privacy & Security**.

Grant access to `Daisy.app`, not only to Terminal. Permission prompts and grants belong to the app's signed identity.

## Trust and paired peers

Open **Paired Peers** from the menu-bar flower to inspect a fingerprint, change a trust policy or forget a peer.

| Policy | Behavior |
|---|---|
| Until 4 days inactive | Expires after 96 hours without a connection; a live connection renews it |
| 30 days | Expires 30 days after pairing; sessions do not extend it |
| This session | Expires when the session ends; an accidental drop gets a 60-second reconnect grace period |
| Until I forget | Does not expire on its own |

Both systems enforce their own policy, so the stricter policy wins. When trust expires, that peer is forgotten and the pair must use a new code. **Forget** ends an active session immediately.

## Launch at login

Choose **Open at Login** in the menu. macOS may require approval in **System Settings → General → Login Items**; Daisy reports that recovery step if registration needs approval.

## Optional command line

The menu-bar app is the default. The same binary keeps CLI commands for diagnostics, automation and advanced network settings:

```bash
alias daisy=/Applications/Daisy.app/Contents/MacOS/daisy

# Inspect permissions and ask macOS for missing grants.
daisy permissions --request

# Optional direct connection; either system can supply input.
daisy listen --pair
daisy connect peer.local --pair --side right

# Use a custom port.
daisy listen --port 24851 --pair
daisy connect peer.local:24851 --pair --side right

# Manage trust and identity.
daisy peers
daisy trust <name-or-fingerprint> 30d
daisy forget <name-or-fingerprint>...
daisy forget --all
daisy rotate-key
daisy id
```

`--side` sets the screen edge where the peer sits. It defaults to the saved arrangement, or right for a peer paired now; when given, it becomes the new choice.

`--bind` controls the listener address; the default is `0.0.0.0`. Use a specific local address when the listener should not accept connections on every interface. When run from Terminal, the bundled binary relaunches through `Daisy.app` so macOS applies the app's permissions.

## Data

The default data directory is:

```text
~/Library/Application Support/daisy/
```

It contains:

- `identity`: this system's long-term private identity key.
- `peers.toml`: paired public keys, names, trust policies, timestamps and screen arrangement.
- `settings.toml`: the last menu-bar setup, stored with mode `0600`.

`--name` changes the name shown to the peer. `--home <directory>`, or `DAISY_HOME`, moves identity, peers and settings storage; this is useful when running two test identities on one system. Backing up or copying `identity` copies the system's identity, so protect it accordingly.

`rotate-key` replaces this system's identity and requires every peer to pair again.

## Trackpad behavior

- A system needs macOS 27 to capture three- and four-finger swipes from its trackpad.
- A macOS 27 peer replays live progress, including pullback cancellation.
- Shake-to-locate replay is verified on macOS 27; macOS 26 visual verification remains.

## Troubleshooting

### A permission remains denied

Open Daisy and use the matching **Grant…** button. Confirm that `Daisy.app` is enabled in the relevant **Privacy & Security** pane. Quit and reopen the app after changing a grant if macOS does not update it immediately.

### An unknown peer is refused

Click **Pair a New Peer** on both sides. **Start Sharing** accepts only peers already paired.

### A connection cannot be opened

Check that the peer is sharing, both systems are on a reachable network, the address is correct and local firewall policy permits TCP port 24850. Daisy reports the address it could not reach instead of retrying, and keeps a listening system available after one bad inbound connection.

### A session stops responding

Daisy ends a session after three seconds of silence and shows **Reconnecting…**; the connecting system tries again until the peer is reachable. If it keeps reconnecting, check that the peer is awake and on the same network. If the address is a raw IP address, it may have changed; a `.local` name keeps working across address changes.

### Input feels wrong

Take control back with Control-Option-Command-Escape. Stop sharing rather than continuing with incorrect input. To collect diagnostics, run a CLI command with `RUST_LOG=daisy=debug` and record which system you were using at each step and the macOS version of each system.

## Remove Daisy

Quit Daisy, remove `Daisy.app`, remove the data directory if you do not need its identity or peer list, and revoke the app in the Accessibility and Input Monitoring permission lists.

See [security-model.md](security-model.md) for the consequences of copying or deleting identity and trust data.
