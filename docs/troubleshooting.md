# Fix a problem

Choose the symptom below. If input is going somewhere unexpected, press **Control-Option-Command-Escape** on the system you are at before investigating.

## Systems keep looking

1. **Check:** Both systems show that sharing is on. If either says **Not Sharing**, click **Start Sharing** there.
2. **Try:** On one system, click **+** beside **Peers**, enter the other's local name or IP address, then **Save and Connect**. Find its IP address in that system's network settings. The attempt starts immediately; leave existing sharing running.
3. **Expect:** A new peer asks for its pairing code; a paired peer connects without one. The title becomes **Connected**.

If the address fails, continue with [An address cannot be reached](#an-address-cannot-be-reached). A successful address connection does not establish why automatic discovery failed; networks can restrict Bonjour even when direct TCP works.

## An address cannot be reached

1. **Check:** The address belongs to the peer's active network connection, the peer is awake and sharing, and both systems can reach each other. Guest or isolated networks may prevent this.
2. **Try:** Correct the saved address with **+** and **Save and Connect**. If you administer the network, check that firewall policy allows TCP port 24850, or the custom port you configured.
3. **Expect:** Daisy reaches the peer and either connects or asks for pairing. An initial failed address attempt reports its error; submit the corrected address to try again.

A `.local` name can follow address changes where local name resolution works. If it does not resolve on your network, use the current IP address.

## Permissions remain denied

1. **Check:** Open **Advanced… → Permissions** in Daisy. Identify the missing grant: Accessibility or Input Monitoring.
2. **Try:** Click **Set Up…** beside it and follow the walkthrough. Enable **Daisy.app** in the System Settings pane it opens. Click **Reopen Daisy** if prompted.
3. **Expect:** Both permissions show as allowed in Daisy, and sharing can start.

If System Settings already shows a grant enabled but Daisy still reports it missing, use **Reset…** beside the permissions, then grant access again. This clears Daisy's permission entries. Granting access only to Terminal does not grant it to Daisy.

## A new peer is refused

1. **Check:** A member that already has peers normally accepts new systems only during an invitation.
2. **Try:** Click **Start Sharing** on the newcomer. On a group member, choose **Add a System** from the menu-bar flower and enter the newcomer's code there.
3. **Expect:** The newcomer joins every member. The invitation closes after success or after 30 seconds; use **Try Again** if it expires.

After a failed or cancelled code prompt, Daisy waits 15 seconds before opening another. See [pairing limits](security-model.md#pairing). If Daisy reports incompatible versions, [update the system it identifies](usage.md#update-daisy).

## A connection keeps dropping

1. **Check:** The peer is awake, still sharing and on a reachable network. Look for an error about expired trust, changed identity or incompatible protocol before treating it as a network failure.
2. **Try:** Restore the network connection. If its IP changed, [submit its current address](usage.md#connect-by-address). For expired trust, [pair again](getting-started.md#pair); for a protocol mismatch, [update Daisy](usage.md#update-daisy).
3. **Expect:** A previously connected, still-trusted peer reconnects without another code after network recovery. For identity errors, use the checks below rather than repeatedly reconnecting.

**If Daisy reports a changed identity:** confirm the address belongs to the intended system and whether its owner deliberately replaced its Daisy identity. If the change was expected, [forget the old peer entry](trust.md#forget-a-peer), then [invite and pair the replacement](usage.md#grow-a-group). Forgetting removes the old peer and its introduced systems from the whole group; they need new pairing to rejoin.

If the change was unexpected or cannot be explained, keep the connection closed and [report it privately](../SECURITY.md). Do not reset identity or approve new pairing just to clear the error.

Daisy ends a silent connection after three seconds to release remote input. Repeated reconnecting is a symptom, not proof of a particular network fault.

## Repeated “Pasting from…” banners

macOS Universal Clipboard can fetch an image while Daisy reads the clipboard, producing repeated banners, especially with a screenshot on the source clipboard.

1. **Check:** Whether Handoff is enabled on the systems sharing the clipboard.
2. **Try:** On each system, open **System Settings → General → AirDrop & Handoff** and turn off **Allow Handoff**. This also disables Universal Clipboard; AirDrop file transfers are separate.
3. **Expect:** Daisy can still share the clipboard when control moves, without Universal Clipboard fetching from another system.

To replace the image already on a clipboard, copy plain text. To empty it, run this on the system holding it:

```bash
pbcopy </dev/null
```

If input also competes with another sharing path, turn off Universal Control in **System Settings → Displays → Advanced…**, using the option that lets your pointer and keyboard move between nearby devices.

## Input goes to the wrong place

1. **Recover:** Press **Control-Option-Command-Escape** on the system you are at. Its pointer returns to the main display and remote held input is released.
2. **Check:** [The shared arrangement](usage.md#arrange-the-screens) matches your desk. Control cannot cross while a mouse button is held or onto a locked peer. Keyboard combinations follow the pointer.
3. **Try:** Correct the arrangement. If behavior is still wrong, choose **Stop Sharing** to keep input local and [report the problem](#report-a-problem).

Control can cross nearby edges and corners, not just the path between the first two displays. See [crossing rules](architecture.md#session-flow).

## A peer's display is locked

Unlock it on that system; Daisy never unlocks a peer or sends input onto a locked display.

While connected, new physical input on the system in control refreshes unlocked members' display idle timers. When input stops, normal display sleep and lock settings apply. If a display sleeps or locks during active use, check that its link is still **Connected**, then [report the behavior](#report-a-problem). See [display activity](macos.md#display-activity) for the mechanism.

## Report a problem

Include:

- Daisy version from **About Daisy** on each system, plus each macOS version.
- What you did, the expected result and the actual result.
- Which system you were using at each step, and whether the peer showed **Connected**, **Locked** or an error.
- Whether you used automatic discovery or an address, and whether the connection ever succeeded.

[Open a bug report](https://github.com/misfitdev/daisy/issues/new). Send suspected vulnerabilities through [private security reporting](../SECURITY.md), rather than a public issue. For debug output, see [command-line diagnostics](usage.md#inspect-permissions-and-connections).
