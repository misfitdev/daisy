# Usage

Daisy shares one Mac's keyboard, mouse and supported trackpad gestures with another Mac on the same network. It does not use an account or cloud service.

## Requirements

- Apple-silicon Macs running macOS 26 or later.
- Accessibility and Input Monitoring granted to the signed `Daisy.app` on both Macs.
- TCP reachability between the Macs. A waiting Mac uses port 24850 by default.
- macOS 27 on the driving Mac to capture trackpad swipes. Keyboard and mouse sharing works on macOS 26.

Install the release in `/Applications` and open `Daisy.app`. Daisy lives in the menu bar. It opens the setup window automatically the first time; afterward, choose **Open Daisy…** from the menu-bar flower.

## Pair two Macs

Pairing is explicit on both Macs. One waits and the other connects, but either can be the Host. Host and Guest describe what happens during this connection; they are not permanent identities for either Mac.

On the first Mac:

1. Choose **Wait for a peer**.
2. Under **Role**, choose **Host** if this Mac's keyboard and trackpad will be shared, or **Guest** if it will receive input.
3. If this Mac is the Host, choose the screen edge that leads to the Guest.
4. Choose a trust policy, or leave the default **Until 4 days inactive**.
5. Click **Pair a New Peer**.

On the second Mac:

1. Choose **Connect by Address**.
2. Choose the waiting Mac from **Nearby**, or enter its local name, such as `studio.local`, or its IP address. Nearby lists paired Macs by name and, while pairing is open, Macs waiting to pair.
3. Choose the other role. Exactly one Mac must be the Host and the other must be the Guest.
4. Click **Pair a New Peer**.

The waiting Mac shows a six-digit code. Enter it on the connecting Mac. The code is used directly as SPAKE2 input; do not compare codes by eye. Pairing closes after the first successful unknown Mac and is also limited to five unknown-key attempts in ten minutes.

## Start and stop sharing

After the Macs are paired, use the same connection and control choices and click **Start Sharing** on both Macs. Choose **Start** or **Stop** from the menu-bar flower for daily use.

Connection and role are independent:

- **Wait for a peer** listens for an incoming network connection.
- **Connect by Address** opens the network connection to the named Mac.
- **Host** uses this Mac's keyboard and trackpad.
- **Guest** receives input from the Host.

A waiting Mac advertises itself with Bonjour, so the other can find it without an address. When a Mac is chosen from Nearby, Daisy finds it again on every connection, wherever its address has moved. The Mac that connects keeps the session going: when either Mac sleeps, wakes or changes network, it finds the other again (or uses the address entered), repeats the encrypted handshake and checks that the same paired Mac answered. Waits between attempts roughly double from 1–2 seconds up to 10–20 seconds, randomized so two Macs do not retry in step, and it keeps trying until the other Mac is back. It stops when you stop sharing, when trust has ended, when a different Mac answers at that address, or on a local error such as Daisy's data folder not being writable. The first connection is not retried: if it fails, Daisy reports the error so a wrong address or a Mac that is not waiting is visible.

## Move control

Push the pointer through the configured shared edge to move control to the following Mac. Push it back out through the corresponding edge to return.

Control never crosses while a mouse button is held. Control-Option-Command-Escape on the driving Mac reclaims it immediately. Disconnect, three seconds of silence, trust revocation or local queue overload also end remote control and release held input.

The menu-bar flower uses color only for state:

- Yellow center: connected.
- Gray center: disconnected, waiting or connecting. The menu text gives the exact state.

## Clipboard

When control crosses, the clipboard goes with it. Copy on the Mac with the keyboard, move to the other Mac and paste there; copy on the other Mac, come back (through the edge or with Control-Option-Command-Escape) and paste at home.

- Plain text, rich text and images are shared. Images copied as TIFF, such as screenshots, arrive as PNG.
- Text and rich text are limited to 4 MB each and images to 32 MB. Anything larger stays on the Mac it was copied on; the rest of the copy still crosses.
- Items marked concealed or transient by their source app stay on the system where they were copied.
- Nothing is sent while control stays on one Mac, and a copy that has already crossed is not sent again.
- A large image is sent behind input, so the pointer never waits for it.

Turn it off with **Share Clipboard** in the menu-bar flower. A Mac with it off neither sends its clipboard nor accepts one. From the command line, add `--no-clipboard` to `listen` or `connect`.

## Discovery

A waiting Mac advertises itself on the local network with Bonjour. The advertisement uses a random name and carries neither the Mac's name nor its key: only a paired Mac can recognise it, and a new advertisement cannot be linked to the last. To stop advertising, turn off **Discoverable on This Network** in the menu-bar flower, or add `--no-discovery` to `listen`. Connecting by name or address keeps working either way, including across networks Bonjour does not reach.

## Permissions

The setup window reports Accessibility and Input Monitoring separately. Choose the corresponding **Grant…** button when either is missing, then finish the change in **System Settings → Privacy & Security**.

Grant access to `Daisy.app`, not only to Terminal. Permission prompts and grants belong to the app's signed identity.

## Trust and paired Macs

Open **Paired Peers** from the menu-bar flower to inspect a fingerprint, change a trust policy or forget a Mac.

| Policy | Behavior |
|---|---|
| Until 4 days inactive | Expires after 96 hours without a connection; a live connection renews it |
| 30 days | Expires 30 days after pairing; sessions do not extend it |
| This session | Expires when the session ends; an accidental drop gets a 60-second reconnect grace period |
| Until I forget | Does not expire on its own |

Both Macs enforce their own policy, so the stricter policy wins. When trust expires, that Mac is forgotten and the pair must use a new code. **Forget** ends an active session immediately.

## Launch at login

Choose **Open at Login** in the menu. macOS may require approval in **System Settings → General → Login Items**; Daisy reports that recovery step if registration needs approval.

## Optional command line

The menu-bar app is the default. The same binary keeps CLI commands for diagnostics, automation and advanced network settings:

```bash
alias daisy=/Applications/Daisy.app/Contents/MacOS/daisy

# Inspect permissions and ask macOS for missing grants.
daisy permissions --request

# Pair: the driving Mac waits and the following Mac connects.
daisy listen --pair --drive left
daisy connect driving-mac.local --pair

# Use a custom port.
daisy listen --port 24851 --pair --drive left
daisy connect driving-mac.local:24851 --pair

# Manage trust and identity.
daisy peers
daisy trust <name-or-fingerprint> 30d
daisy forget <name-or-fingerprint>...
daisy forget --all
daisy rotate-key
daisy id
```

`--bind` controls the listener address; the default is `0.0.0.0`. Use a specific local address when the listener should not accept connections on every interface. When run from Terminal, the bundled binary relaunches through `Daisy.app` so macOS applies the app's permissions.

## Data

The default data directory is:

```text
~/Library/Application Support/daisy/
```

It contains:

- `identity`: this Mac's long-term private identity key.
- `peers.toml`: paired public keys, names, trust policies and timestamps.
- `settings.toml`: the last menu-bar setup, stored with mode `0600`.

`--name` changes the name shown to the other Mac. `--home <directory>`, or `DAISY_HOME`, moves identity, peers and settings storage; this is useful when running two test identities on one Mac. Backing up or copying `identity` copies the Mac's identity, so protect it accordingly.

`rotate-key` replaces this Mac's identity and requires every peer to pair again.

## Trackpad behavior

- The driving Mac needs macOS 27 to capture three- and four-finger swipes.
- A macOS 27 follower replays live progress, including pullback cancellation.
- Shake-to-locate replay is verified on macOS 27; macOS 26 visual verification remains.

## Troubleshooting

### A permission remains denied

Open Daisy and use the matching **Grant…** button. Confirm that `Daisy.app` is enabled in the relevant **Privacy & Security** pane. Quit and reopen the app after changing a grant if macOS does not update it immediately.

### An unknown Mac is refused

Click **Pair a New Peer** on both sides. **Start Sharing** accepts only Macs already paired.

### Daisy asks for one Host and one Guest

Choose **Host** on exactly one Mac and **Guest** on the other. Either role can wait or connect.

### A connection cannot be opened

Check that the other Mac is waiting, both Macs are on a reachable network, the address is correct and local firewall policy permits TCP port 24850. Daisy reports the address it could not reach instead of retrying, and keeps a listening Mac available after one bad inbound connection.

### A session stops responding

Daisy ends a session after three seconds of silence and shows **Reconnecting…**; the connecting Mac tries again until the other Mac is reachable. If it keeps reconnecting, check that the other Mac is awake and on the same network. If the address is a raw IP address, it may have changed; a `.local` name keeps working across address changes.

### Input feels wrong

Take control back with Control-Option-Command-Escape. Stop sharing rather than continuing with incorrect input. To collect diagnostics, run a CLI command with `RUST_LOG=daisy=debug` and record the role and macOS version of each Mac.

## Remove Daisy

Quit Daisy, remove `Daisy.app`, remove the data directory if you do not need its identity or peer list, and revoke the app in the Accessibility and Input Monitoring permission lists.

See [security-model.md](security-model.md) for the consequences of copying or deleting identity and trust data.
