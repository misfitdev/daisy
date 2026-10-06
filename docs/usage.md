# Usage

Daisy shares one keyboard, mouse and supported trackpad gestures across a group of up to eight systems on the same network. It does not use an account or cloud service. Find a specific task in the [documentation index](README.md).

## Get started

Follow [Install → Start → Pair](getting-started.md) to connect your first two systems. Already paired? Choose **Start Sharing** on both; trusted peers reconnect automatically.

For setup or connection problems, use [Troubleshooting](troubleshooting.md).

## Start and stop sharing

Choose **Start Sharing** or **Stop Sharing** in the Daisy window or menu-bar flower. Stopping sharing closes every group connection and releases held remote input. Daisy remembers your choice and resumes sharing after login or restart if it was on.

Choose **Open Daisy** from the menu-bar flower to open its window. Command-W or Escape closes the window or sheet. Command-Q closes Daisy's windows and leaves sharing running; choose **Quit** from the menu-bar flower to quit the app.

**About Daisy**, in the app menu while its window is open, shows the version and build commit. `daisy --version` prints the same information.

## Connect by address

If automatic discovery does not find a peer, click **+** beside **Peers**, enter its local name or IP address, then click **Save and Connect**. The peer must be sharing. Daisy saves the address and starts connecting immediately, keeping existing connections and automatic discovery active.

Both systems must be reachable over TCP port 24850, the default. For a new peer, enter its pairing code when prompted. For a paired peer, Daisy checks the saved identity and trust before connecting.

## Arrange the screens

Open Daisy. The arrangement at the top shows every display in the group.

**With a pointer:** drag the peer's displays to match your desk. Its displays move together and snap near another edge or corner. Displays from different systems cannot overlap.

**With a keyboard:** select the arrangement, press **[** or **]** to choose a peer, then use the **arrow keys** to place it against that side.

Changes apply across the group and are saved. If two systems change the arrangement, the later change wins. Adding, removing or rearranging a display updates the layout and moves any system that would overlap.

[Move control](#move-control) across any nearby edge or corner; the arrangement is not a single chain.

## Move control

Move the pointer off a display toward a peer's display to use that system. You can cross any nearby edge or corner; crossing between this system's own displays stays local. Small gaps are allowed. See [crossing geometry](architecture.md#session-flow) for the exact rules.

Using a system's own keyboard or trackpad takes control there immediately. Momentum scrolling stays on the system where the flick began. Control never crosses while a mouse button is held or onto a locked system; unlock it there first.

**Control-Option-Command-Escape** returns control to the system you are at and centers its pointer on the main display. Disconnecting or stopping sharing also releases held remote input.

Check the window title for **Connected**, **Not Sharing** or the current connection state. Under **Peers**, each row shows **Connected** with round-trip latency, **Not connected**, or **Locked**. The system in control has a coral daisy in the arrangement. The menu-bar flower has a yellow center when connected and a gray center otherwise.

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

It is on by default. Turn it off with **Share clipboard when control moves** under **Advanced…**. A system with it off neither sends its clipboard nor accepts one. From the command line, add `--no-clipboard` to `listen` or `connect`.

## Grow a group

1. Click **Start Sharing** on the new system.
2. On a member already sharing, choose **Add a System** from the menu-bar flower.
3. Enter the new system's code on that member and click **Connect**.

The new system joins every member without another code. A group supports up to eight systems. Sharing continues during pairing, including connections made by address.

The invitation lasts 30 seconds. Use **Cancel** to close it or **Try Again** if no system joins. After a failed or cancelled code prompt, Daisy waits 15 seconds before asking again.

To accept new systems whenever sharing is on, enable **Always discoverable** under **Advanced…**. Turning it on requires confirmation and Touch ID or the login password. While enabled, **Add a System** is unavailable. Each system keeps its own setting; an organization can [disable it](administration.md).

A peer that sleeps or leaves the network drops out without interrupting the rest, then reconnects when available and still trusted. While connected, physical input on the system in control keeps unlocked members' displays active. When input stops, normal display sleep and lock timers apply. Daisy never unlocks a system.

Pairing one member admits a system to the whole group. See [group trust and introductions](security-model.md#groups) before adding a system you do not control.

## Trust and paired peers

Under **Peers**, click a peer's trust-duration button, such as **Until unused 4 days…**, to change its policy. Hover over its name to see the key fingerprint.

[Manage trust](trust.md) explains expiration, group access and forgetting. **Forget…** removes the peer and its introduced systems from the whole group. Use **Stop Sharing** for a temporary stop that retains peers.

## Troubleshooting

[Choose your symptom](troubleshooting.md): systems keep looking, an address fails, permissions remain denied, a connection drops, clipboard banners repeat or input goes to the wrong place.

## Permissions

The **Permissions** group under **Advanced…** lists Accessibility and Input Monitoring. **Set Up…** beside a missing one opens the walkthrough, which finishes the change in **System Settings → Privacy & Security**. Grant access to `Daisy.app`, not only to Terminal. Permission prompts and grants belong to the app's signed identity. **Reset…** beside them clears Daisy's entries when System Settings shows them on but they do not apply.

## Launch at login

Daisy opens at login by default; turn off **Open at login** under **Advanced…** to stop it. macOS may require approval in **System Settings → General → Login Items**; Daisy reports that recovery step if registration needs approval.

## Discovery

Paired peers find each other automatically with Bonjour while sharing. If your network does not carry Bonjour, [connect by address](#connect-by-address).

From the command line, `listen --no-discovery` disables advertising; connections by name or address still work. See [discovery privacy](security-model.md#discovery) for what advertisements contain.

## Update Daisy

Quit Daisy, install the [latest release](https://github.com/misfitdev/daisy/releases/latest) in Applications and reopen it. Settings and paired trust are retained. If you installed with Homebrew, use the [Homebrew upgrade steps](#homebrew).

Different releases can connect when their protocol versions match. If Daisy reports a protocol mismatch, update the system it identifies; updating all group members together avoids mixed versions. See the [protocol compatibility rules](protocol.md#handshake).

### Install staged update

To install a newer signed release without copying over the running app, mount its DMG and run:

```bash
/Applications/Daisy.app/Contents/MacOS/daisy install-update /Volumes/Daisy/Daisy.app
```

Daisy stages and verifies the replacement while the current copy keeps running. It then stops the current process, checks that the network listener has closed, exchanges the complete app bundles, and starts the replacement. Sharing resumes with the same device identity, paired peers, settings and screen arrangement. The previous copy stays available until startup succeeds. If startup fails, Daisy stops the replacement and restores the previous copy. The command accepts only a newer notarized release from the same publisher using the same network protocol. Protocol changes still require a manual group update. This command does not enable automatic installation.

## Homebrew

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

Reopen Daisy afterward. See [update compatibility](#update-daisy) if a peer reports a protocol mismatch.
Homebrew checks the DMG's SHA-256 before installing. It does not grant
Accessibility or Input Monitoring, and Daisy does not update itself.
`brew uninstall --cask local/daisy/daisy` removes the app while retaining
settings, device identity, and paired peers. If Daisy was installed manually,
quit it and remove the existing app from Applications before installing the cask.


## Optional command line

The same app binary provides commands for diagnostics, automation and advanced network settings. Run this once in a Terminal session to use the examples below:

```bash
alias daisy=/Applications/Daisy.app/Contents/MacOS/daisy
```

When run from Terminal, the bundled binary relaunches through `Daisy.app` so macOS applies the app's permissions. Quit the running menu-bar app before starting a CLI sharing session.

### Inspect permissions and connections

```bash
# Inspect permissions and ask macOS for missing grants.
daisy permissions --request
# List trusted peers, round-trip statistics and local identity.
daisy peers
daisy stats
daisy id
```

To collect debug output, prefix a sharing command with `RUST_LOG=daisy=debug`, for example `RUST_LOG=daisy=debug daisy listen`. Record the system in use and macOS versions when [reporting a problem](troubleshooting.md#report-a-problem).

### Connect directly or use a custom port

Run `listen` on one system and `connect` on the other. Add `--pair` for a new peer:

```bash
daisy listen --pair
daisy connect peer.local --pair --side right

# Both sides must use the same custom port.
daisy listen --port 24851 --pair
daisy connect peer.local:24851 --pair --side right
```

`--side` places a peer that has no saved position; it defaults to right. Arrange it in the window afterward. `--bind` sets the listener address; the default, `0.0.0.0`, accepts connections on every interface. Use a specific local address to restrict it.

### Change trust from the command line

Replace `<name-or-fingerprint>` with a peer name or fingerprint from `daisy peers`.

```bash
daisy trust <name-or-fingerprint> idle:12h
```

| Policy argument | Effect |
|---|---|
| `idle` | Trust until unused for 4 days |
| `idle:<hours>h` or `idle:<days>d` | Trust until unused for that duration |
| `<days>d` | Fixed term from this change; connecting does not extend it |
| `once` | This session, with accidental-drop reconnect grace |
| `forever` | Until explicitly forgotten |

[Trust rules](trust.md#change-how-long-trust-lasts) still apply on both systems.

### Forget peers or leave the group

```bash
# Remove named peers and their introduced systems from the whole group.
daisy forget <name-or-fingerprint>...
# Leave from this system without removing other group members.
daisy forget --all
```

Named forgetting requires new pairing for removed systems to rejoin. To pause access without changing trust, stop sharing instead.

### Inspect or replace identity

```bash
daisy id
# Replace both local identities; every peer must pair again.
daisy rotate-key
```

Unlock this system before rotating its device key. See [key protection](security-model.md#keys-at-rest) before copying or deleting identity files.

## Data

Daisy keeps settings, paired peers and display arrangement in `~/Library/Application Support/daisy/`. Its device signing key stays in this system's Secure Enclave; copying the directory does not copy a usable device identity.

Use `--home <directory>` or `DAISY_HOME` to choose a different data directory, and `--name` to change the name peers see. For identity rotation, see [identity commands](#inspect-or-replace-identity). Unlock this system before creating its device key.

See [saved files](architecture.md#saved-state) and [key protection](security-model.md#keys-at-rest) before copying or deleting identity and trust data.

## Trackpad behavior

- Daisy captures three- and four-finger trackpad swipes on macOS 26 and later.
- A macOS 27 peer replays live progress, including pullback cancellation.

## Remove Daisy

Quit Daisy, remove `Daisy.app`, remove the data directory if you do not need its identity or peer list, and revoke the app in the Accessibility and Input Monitoring permission lists.

See [security-model.md](security-model.md) for the consequences of copying or deleting identity and trust data.
