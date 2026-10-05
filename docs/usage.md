# Usage

Daisy shares one keyboard, mouse and supported trackpad gestures across a group of up to eight systems on the same network. It does not use an account or cloud service.

## Update the group

Daisy 0.6.0 uses protocol 6. Update every member before sharing; 0.5.0 cannot connect to 0.6.0. Existing device identities and paired trust settings are retained.

## Requirements

- Apple silicon systems running macOS 26 or later.
- Accessibility and Input Monitoring granted to the signed `Daisy.app` on every system.
- TCP reachability between the systems. Daisy listens on port 24850 by default.
- Trackpad swipes work from macOS 26 and later.

## Install

Open the release DMG and drag Daisy to Applications. Opened from anywhere else after downloading, Daisy offers to move itself to Applications, replacing an older copy there and keeping its settings and paired peers, then reopens from there.

### Homebrew

Each release includes a `daisy.rb` cask that installs the
same signed and notarized app. You can use it in a local tap without a published
Daisy tap. Create the local tap once:

```bash
brew tap-new --no-git local/daisy
mkdir -p "$(brew --repository local/daisy)/Casks"
```

Download the recipe for the version you want and install it:

```bash
curl --fail --location https://github.com/misfitdev/daisy/releases/download/v0.6.0/daisy.rb \
  --output "$(brew --repository local/daisy)/Casks/daisy.rb"
brew install --cask local/daisy/daisy
```

For a later release, download its recipe from that release's tag using the same
command with the new version. Quit Daisy, then run:

```bash
brew upgrade --cask local/daisy/daisy
```

Reopen Daisy afterward. Update every group member to a compatible version.
Homebrew checks the DMG's SHA-256 before installing. It does not grant
Accessibility or Input Monitoring, and Daisy does not update itself.
`brew uninstall --cask local/daisy/daisy` removes the app while retaining
settings, device identity, and paired peers. If Daisy was installed manually,
quit it and remove the existing app from Applications before installing the cask.

## Open Daisy

Daisy lives in the menu bar. While Accessibility or Input Monitoring is missing, it opens a walkthrough that asks for each in turn, opens the matching System Settings pane and moves on once it is switched on. macOS can apply Input Monitoring only after Daisy reopens; the walkthrough offers **Reopen Daisy** when that is needed, then continues. Afterward, choose **Open Daisy** from the menu-bar flower.

The menu-bar flower holds **Open Daisy**, **Start Sharing** or **Stop Sharing**, **Add a System** and **Quit**. Command-W or Escape closes a Daisy window or sheet. Command-Q closes Daisy's windows and leaves sharing running; **Quit** in the menu-bar flower quits. **About Daisy**, in the app menu while a Daisy window is open, shows the version and the commit it was built from; `daisy --version` prints the same.

## Start and stop sharing

Daisy starts out **Not Sharing**. **Start Sharing** and **Stop Sharing**, in the window or the menu-bar flower, turn sharing on and off. Daisy remembers which, and after login or a restart it resumes sharing if it was on.

While sharing, Daisy advertises with Bonjour, and trusted members nearby find each other automatically, repeating the encrypted handshake and checking trust after a disconnect. For each pair, the lower public key opens the connection and the other listens. Any member can supply input regardless of which opened a connection. Stopping sharing ends every link; trust revocation also ends a member's link.

## Pair two systems

Click **Start Sharing** on both systems. A system with no peers is open to pairing for as long as it is sharing, so the two find each other over Bonjour.

The system that started sharing later shows a six-digit code in a small Daisy panel. Type it in the panel on the other system and click **Connect**. If it does not match, the panel says "That code didn't match" and Daisy asks again shortly with a new code. An unknown peer is limited to five attempts in ten minutes.

One code pairs both systems. Each trusts the other until it goes unused for four days; change that from the peer's row in **Peers**. A "Connected to" panel follows: **Arrange…** opens the window with the new peer selected, and the panel closes on its own after 15 seconds.

For a network without Bonjour, click **+** beside **Peers**, enter the peer’s local name or IP address, then click **Save and Connect**. Daisy saves the address and starts connecting immediately; the other system must be sharing. Existing group links stay connected, and automatic discovery keeps running. A direct connection backs off after an established session drops; Bonjour discovery keeps looking for every trusted member.

## Grow a group

To add a system, click **Start Sharing** on it. On any one system already in the group, choose **Add a System** from the menu-bar flower; the group then accepts a new system for 30 seconds, and the panel there counts down and offers **Cancel**. The new system shows the code; type it on the member. If nothing joins in time, the panel says "No new system joined" and offers **Try Again**. **Cancel** on the code prompt also ends Add a System, and Daisy waits 15 seconds before asking again. Sharing keeps running throughout, including when connecting by address.

That member introduces it to the rest with a signed introduction, so every member trusts it and links to it without another code; see the [security model](security-model.md#groups). A group holds up to eight systems; a ninth is refused. Each member trusts an introduced system for no longer than it trusts the member that introduced it.

Every member links directly with every other. A system that sleeps or leaves the network drops out; the rest keep working, and it rejoins on its own once it is awake, unlocked and still trusted. While a group runs, each member holds off idle system sleep. New physical input on the system in control refreshes every unlocked member’s display idle timeout. Control arriving on a system also wakes its display. When input stops, each system’s normal display sleep and lock settings apply; Daisy does not unlock a locked system.

To let the group accept a new system at any time without **Add a System**, turn on **Always discoverable** under **Advanced…**. Daisy asks you to confirm, then asks for Touch ID or the login password. While it is on, **Add a System** is unavailable. Turning it off needs nothing. Each system keeps its own setting, and it applies only while sharing. An organization can turn it off with a configuration profile; see [administration](administration.md).

A group member always types the code and the new system shows it. Between two systems with no peers, the one that has been open to pairing longer types; if they opened within two seconds of each other, a random value decides.

## Arrange the screens

The top of the window shows every display of every system in the group, as Displays shows monitors. Drag a peer's displays anywhere: they move together, snap when an edge comes within a few points of another, and never overlap another system's displays. Without a pointer, select the arrangement, press [ or ] to choose a peer, and use the arrow keys to put it against that side. Changes apply across the group at once and are kept for next time; when two systems change it, the later change wins. Plugging in, removing or rearranging a display updates the arrangement, moving any system that would now overlap.

## Move control

Push the pointer off any of this system's displays toward another system's display to use that system. Small gaps and corners still connect, as long as the next display is within 40 points along the way. The pointer goes from system to system the same way, and back. Moving between one system's own displays never crosses.

Using any system's own keyboard or trackpad immediately takes control there and releases remote held input; momentum scrolling after a trackpad flick does not, and coasts only on the system where the flick began. A 150 ms settle window limits repeated claims during simultaneous use; local input always stays local during that window. The arrangement stays the same when control changes.

Control never crosses while a mouse button is held, and never onto a system whose screen is locked; unlock it there first. Control-Option-Command-Escape immediately returns control to the system you are at and puts the pointer in the middle of its main display; use it any time to find the pointer where you are looking. Disconnect, three seconds of silence, trust revocation or local queue overload also end remote control and release held input.

The menu-bar flower uses color only for state:

- Yellow center: connected.
- Gray center: not sharing, looking for peers, or connecting.

The window gives the exact state in its title. The system in control is coral with the daisy; each peer's row shows whether its link is running and its round trip, whether its screen is locked, and who introduced it, for example "via Studio · Connected, 12 ms", "Locked" or "Not connected". A round trip is measured from the heartbeat each system sends every second; a single round trip over 50 ms is written to the log. `daisy stats` prints each running link's round trips by percentile.

## Keyboard

Keys and keyboard combinations follow the pointer. Shift, Control, Option,
Command and Fn retain their modifier state on forwarded key presses, repeats
and releases. Both left and right modifier keys work independently where the
keyboard provides them. Caps Lock remains a toggle.

Stopping sharing releases held remote keys and modifiers and closes every group
link. Control-Option-Command-Escape remains the shortcut to take control back.

## Clipboard

When control moves, the clipboard goes with it: the system giving up control sends its clipboard, including when control moves because someone started using the other system. Copy on one system, move to the peer and paste there; copy on the peer, come back (through the edge, by using this system, or with Control-Option-Command-Escape) and paste here.

- Plain text, rich text and images are shared. Images copied as TIFF, such as screenshots, arrive as PNG.
- Text and rich text are limited to 4 MB each and images to 32 MB. Anything larger stays on the system it was copied on; the rest of the copy still crosses.
- Items marked concealed or transient by their source app stay on the system where they were copied.
- Nothing is sent while control stays on one system, and a copy that has already crossed is not sent again.
- A large image is sent behind input, so the pointer never waits for it.

It is on by default. Turn it off with **Share clipboard when control moves** under **Advanced…**. A system with it off neither sends its clipboard nor accepts one. From the command line, add `--no-clipboard` to `listen` or `connect`.

## Discovery

Each system advertises itself on the local network with Bonjour. The advertisement uses a random name and carries neither the system's name nor its key: only a paired peer can recognise it, and a new advertisement cannot be linked to the last. The app advertises whenever it is sharing. From the command line, add `--no-discovery` to `listen` to stop advertising. Connecting by name or address keeps working either way, including across networks Bonjour does not reach.

## Permissions

The **Permissions** group under **Advanced…** lists Accessibility and Input Monitoring. **Set Up…** beside a missing one opens the walkthrough, which finishes the change in **System Settings → Privacy & Security**. Grant access to `Daisy.app`, not only to Terminal. Permission prompts and grants belong to the app's signed identity. **Reset…** beside them clears Daisy's entries when System Settings shows them on but they do not apply.

## Trust and paired peers

Each system trusts a new peer until it goes unused for four days. The **Peers** group lists each paired peer; its key fingerprint is in the help tag on its name. Click its trust to change it, or **Forget…**. Changing trust starts it over from that moment.

| Choice | Behavior |
|---|---|
| Until unused for 4 days (the default; any number of hours or days) | Expires after that long without a connection; a live connection renews it |
| This session | Expires when the session ends; an accidental drop gets a 60-second reconnect grace period |
| Until I forget | Does not expire on its own |

Both systems enforce their own choice, so the stricter one wins. When trust expires, that peer is forgotten and the pair must use a new code. Trust in a system introduced by a member ends when trust in that member ends.

**Forget…** ends that system's link immediately and removes it from the whole group: every member drops it, and every system it introduced, and a member that has not heard yet cannot bring it back. `daisy forget --all` only makes this system leave; it removes no one from the group.

## Launch at login

Daisy opens at login by default; turn off **Open at login** under **Advanced…** to stop it. macOS may require approval in **System Settings → General → Login Items**; Daisy reports that recovery step if registration needs approval.

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
- `device-identity`: a reference to its Secure Enclave device signing key in the Keychain.
- `trust-v5/peers.toml`: trusted public keys, names, trust policies, timestamps, signing keys and who introduced each.
- `trust-v5/revocations.toml`: signed revocations to pass on to members.
- `trust-v5/arrangement.toml`: where every member's displays were last placed.
- `stats.toml`: each running link's round trips, for `daisy stats`.
- `settings.toml`: the last menu-bar setup, stored with mode `0600`.

`--name` changes the name shown to the peer. `--home <directory>`, or `DAISY_HOME`, moves identity, peers and settings storage; this is useful when running two test identities on one system. The device private key stays in this system’s Secure Enclave. Copying these files to another system does not copy that identity; reset the identity and pair again.

Unlock this system before creating or rotating its device key. Existing keys can sign after the first unlock without a Touch ID or password prompt.

`rotate-key` replaces this system's identity and signing key and requires every peer to pair again.

## Trackpad behavior

- Daisy captures three- and four-finger trackpad swipes on macOS 26 and later.
- A macOS 27 peer replays live progress, including pullback cancellation.

## Troubleshooting

### Repeated clipboard banners or competing input sharing

macOS Universal Clipboard can fetch an image from another system when Daisy
reads the clipboard to share it. This can show a repeated "Pasting from…"
banner, especially when the source clipboard contains a screenshot.

When Daisy handles input and clipboard sharing, turn off the overlapping Apple
features on each system:

- In **System Settings → Displays → Advanced…**, turn off Universal Control
  (the option allowing your pointer and keyboard to move between nearby devices).
- In **System Settings → General → AirDrop & Handoff**, turn off **Allow
  Handoff**. This disables Universal
  Clipboard too; AirDrop file transfers are a separate feature.

Copy plain text to replace an image already on the clipboard, or run
`pbcopy </dev/null` in Terminal to empty it. Daisy's clipboard sharing remains
available with Handoff off.

### A permission remains denied

Open Daisy, click **Advanced…** and use the matching **Set Up…** button. Confirm that `Daisy.app` is enabled in the relevant **Privacy & Security** pane. Quit and reopen the app after changing a grant if macOS does not update it immediately.

### An unknown peer is refused

A system with peers accepts a new one only during **Add a System** or while **Always discoverable** is on. Click **Start Sharing** on the new system, then choose **Add a System** on a member.

### A connection cannot be opened

Check that the peer is sharing, both systems are on a reachable network, the address is correct and local firewall policy permits TCP port 24850. Daisy reports the address it could not reach instead of retrying, and keeps a listening system available after one bad inbound connection.

### A session stops responding

Daisy ends a session after three seconds of silence and shows **Reconnecting…**; the connecting system tries again until the peer is reachable. If it keeps reconnecting, check that the peer is awake and on the same network. If the address is a raw IP address, it may have changed; a `.local` name keeps working across address changes.

### Input feels wrong

Take control back with Control-Option-Command-Escape. Stop sharing rather than continuing with incorrect input. To collect diagnostics, run a CLI command with `RUST_LOG=daisy=debug` and record which system you were using at each step and the macOS version of each system.

## Remove Daisy

Quit Daisy, remove `Daisy.app`, remove the data directory if you do not need its identity or peer list, and revoke the app in the Accessibility and Input Monitoring permission lists.

See [security-model.md](security-model.md) for the consequences of copying or deleting identity and trust data.
