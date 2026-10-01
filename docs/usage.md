# Usage

Daisy shares one keyboard, mouse and supported trackpad gestures across a group of up to eight systems on the same network. It does not use an account or cloud service.

## Requirements

- Apple silicon systems running macOS 26 or later.
- Accessibility and Input Monitoring granted to the signed `Daisy.app` on every system.
- TCP reachability between the systems. Daisy listens on port 24850 by default.
- macOS 27 on the system whose trackpad swipes should cross. Keyboard and mouse sharing works on macOS 26.

Open the release DMG and drag Daisy to Applications. Opened from anywhere else after downloading, Daisy offers to move itself to Applications, replacing an older copy there and keeping its settings and paired peers, then reopens from there.

Daisy lives in the menu bar. While Accessibility or Input Monitoring is missing, it opens a walkthrough that asks for each in turn, opens the matching System Settings pane and moves on once it is switched on. macOS can apply Input Monitoring only after Daisy reopens; the walkthrough offers **Reopen Daisy** when that is needed, then continues to pairing. Afterward, choose **Open Daisy…** from the menu-bar flower.

Command-W or Escape closes a Daisy window or sheet. Command-Q also closes Daisy's windows and leaves sharing running; turn on **⌘Q quits Daisy** in the Daisy window to make it quit instead. **Quit Daisy** in the menu-bar flower always quits.

## Pair two systems

Click **Pair a New Peer…** on both systems. Nearby systems find each other automatically over Bonjour. The connection opener is chosen deterministically; you do not choose a network or input role.

One system shows a six-digit code. Enter it on the other. The code is used directly as SPAKE2 input; do not compare codes by eye. Pairing closes after the first success, and an unknown peer is limited to five unknown-key attempts in ten minutes.

Once paired, each system asks how long to trust the other. Its displays then appear beside this system's at the top of the window.

For a network without Bonjour, click **Advanced…** and enter the peer's local name or IP address on one system, leaving the address empty on the other. The **Nearby** menu there can also pick a particular peer.

## Grow a group

To add a system, pair it with any one system already in the group. That member introduces it to the rest with a signed introduction, so every member trusts it and links to it without another code; see the [security model](security-model.md#groups). A group holds up to eight systems; a ninth is refused. Each member trusts an introduced system for no longer than it trusts the member that introduced it.

Every member links directly with every other. A system that sleeps or leaves the network drops out; the rest keep working, and it rejoins on its own once it is awake, unlocked and still trusted. While a group runs, each member holds off idle sleep, though its display may still sleep; control arriving on a system wakes its display.

## Start and stop sharing

Click **Start Sharing** on each system, or choose **Start Sharing** from the menu-bar flower. Trusted members nearby advertise and find each other automatically, repeating the encrypted handshake and checking trust after a disconnect. For each pair, the lower public key opens the connection and the other listens. Any member can supply input regardless of which opened a connection.

An address under **Advanced…** connects directly, which is useful when Bonjour cannot reach a peer. When a paired peer is picked from **Nearby**, Daisy checks its pinned key and discovers its current address on reconnect. Direct connections back off after an established session drops; the automatic nearby workflow continues looking for every trusted member.

Choose **Stop Sharing** to end sharing. Trust revocation also ends a member's link.

## Arrange the screens

The top of the window shows every display of every system in the group, as Displays shows monitors. Drag a peer's displays anywhere: they move together, snap when an edge comes within a few points of another, and never overlap another system's displays. Without a pointer, select the arrangement, press [ or ] to choose a peer, and use the arrow keys to put it against that side. Changes apply across the group at once and are kept for next time; when two systems change it, the later change wins. Plugging in, removing or rearranging a display updates the arrangement, moving any system that would now overlap.

## Move control

Push the pointer off any of this system's displays toward another system's display to use that system. Small gaps and corners still connect, as long as the next display is within 40 points along the way. The pointer goes from system to system the same way, and back. Moving between one system's own displays never crosses.

Using any system's own keyboard or trackpad immediately takes control there and releases remote held input; momentum scrolling after a trackpad flick does not, and coasts only on the system where the flick began. A 150 ms settle window limits repeated claims during simultaneous use; local input always stays local during that window. The arrangement stays the same when control changes.

Control never crosses while a mouse button is held, and never onto a system whose screen is locked; unlock it there first. Control-Option-Command-Escape immediately returns control to the system you are at. Disconnect, three seconds of silence, trust revocation or local queue overload also end remote control and release held input.

The menu-bar flower uses color only for state:

- Yellow center: connected.
- Gray center: stopped, looking for peers, or connecting.

The menu text gives the exact state. While connected, it names the members and adds each round trip, for example "Connected to Studio · 4 ms". In the window, the system in control is coral with the daisy; each peer's row shows whether its link is running, its round trip, whether its screen is locked, and who introduced it. A round trip is measured from the heartbeat each system sends every second; a single round trip over 50 ms is written to the log. `daisy stats` prints each running link's round trips by percentile.

## Clipboard

When control moves, the clipboard goes with it: the system giving up control sends its clipboard, including when control moves because someone started using the other system. Copy on one system, move to the peer and paste there; copy on the peer, come back (through the edge, by using this system, or with Control-Option-Command-Escape) and paste here.

- Plain text, rich text and images are shared. Images copied as TIFF, such as screenshots, arrive as PNG.
- Text and rich text are limited to 4 MB each and images to 32 MB. Anything larger stays on the system it was copied on; the rest of the copy still crosses.
- Items marked concealed or transient by their source app stay on the system where they were copied.
- Nothing is sent while control stays on one system, and a copy that has already crossed is not sent again.
- A large image is sent behind input, so the pointer never waits for it.

Turn it off with **Share clipboard when control moves** in the Daisy window. A system with it off neither sends its clipboard nor accepts one. From the command line, add `--no-clipboard` to `listen` or `connect`.

## Discovery

Each system advertises itself on the local network with Bonjour. The advertisement uses a random name and carries neither the system's name nor its key: only a paired peer can recognise it, and a new advertisement cannot be linked to the last. To stop advertising, turn off **Discoverable on this network** under **Advanced…**, or add `--no-discovery` to `listen`. Connecting by name or address keeps working either way, including across networks Bonjour does not reach.

## Permissions

The **Permissions** group lists Accessibility and Input Monitoring. **Set Up…** beside a missing one opens the walkthrough, which finishes the change in **System Settings → Privacy & Security**. Grant access to `Daisy.app`, not only to Terminal. Permission prompts and grants belong to the app's signed identity. **Reset…** under **Advanced…** clears Daisy's entries when System Settings shows them on but they do not apply.

## Trust and paired peers

Each system chooses how long to trust a peer when they pair. The **Peers** group lists each paired peer with its fingerprint; click its trust to change it, or **Forget…**. Changing trust starts it over from that moment.

| Choice | Behavior |
|---|---|
| Until unused for 4 days (the default; any number of hours or days) | Expires after that long without a connection; a live connection renews it |
| This session | Expires when the session ends; an accidental drop gets a 60-second reconnect grace period |
| Until I forget | Does not expire on its own |

Both systems enforce their own choice, so the stricter one wins. When trust expires, that peer is forgotten and the pair must use a new code. Trust in a system introduced by a member ends when trust in that member ends.

**Forget…** ends that system's link immediately and removes it from the whole group: every member drops it, and every system it introduced, and a member that has not heard yet cannot bring it back. `daisy forget --all` only makes this system leave; it removes no one from the group.

## Launch at login

Turn on **Open at login** in the Daisy window. macOS may require approval in **System Settings → General → Login Items**; Daisy reports that recovery step if registration needs approval.

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
daisy stats
daisy trust <name-or-fingerprint> idle:12h   # or idle (4 days), idle:<days>d, <days>d, once, forever
daisy forget <name-or-fingerprint>...
daisy forget --all
daisy rotate-key
daisy id
```

`--side` sets the side a peer is placed on when it has no place in the arrangement yet; it defaults to right. The window can place it anywhere afterward.

`--bind` controls the listener address; the default is `0.0.0.0`. Use a specific local address when the listener should not accept connections on every interface. When run from Terminal, the bundled binary relaunches through `Daisy.app` so macOS applies the app's permissions.

## Data

The default data directory is:

```text
~/Library/Application Support/daisy/
```

It contains:

- `identity`: this system's long-term private identity key.
- `signing`: the key it signs introductions and revocations with.
- `peers.toml`: trusted public keys, names, trust policies, timestamps, signing keys and who introduced each.
- `revocations.toml`: signed revocations to pass on to members.
- `arrangement.toml`: where every member's displays were last placed.
- `stats.toml`: each running link's round trips, for `daisy stats`.
- `settings.toml`: the last menu-bar setup, stored with mode `0600`.

`--name` changes the name shown to the peer. `--home <directory>`, or `DAISY_HOME`, moves identity, peers and settings storage; this is useful when running two test identities on one system. Backing up or copying `identity` copies the system's identity, so protect it accordingly.

`rotate-key` replaces this system's identity and signing key and requires every peer to pair again.

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
