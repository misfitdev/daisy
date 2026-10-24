# Manage trust

Pairing lets every member type and click on every other member's system. Members can also introduce new systems to the group. Only pair systems you trust with that access.

## Pair a system

For your first pair, follow [Getting started](getting-started.md). For an existing group, [add a system](usage.md#grow-a-group) through any member. The newcomer joins the whole group without another code.

Paired systems recognize each other using their network and Secure Enclave device keys. Copying Daisy's files to another system does not copy a usable device identity. See the [Security model](security-model.md#connections) for the authentication checks.

## Change how long trust lasts

Open Daisy and find the peer under **Peers**. Click its **Remember…** button, choose an option and click **Save**. The new duration starts from that moment.

| Choice | Trust ends when |
|---|---|
| Until unused for (4 days by default) | That many hours or days pass without a connection. Staying connected renews it. |
| End of session | The peer’s last group connection ends. Closing one connection keeps trust while another group connection continues. An accidental drop allows 60 seconds to reconnect. |
| Forever | You forget the peer. |

Membership and retention choices are shared across the group. Changing **Remember…** on any member updates that peer’s policy throughout the connected group; systems that were away catch up when they reconnect. An active connection anywhere in the group renews idle trust. Each system enforces the shared choice using its own clock. When trust expires, pair again with a new code. Other members keep their trust when the system that added them leaves.

A fixed deadline set from the command line appears as **N days from pairing**.
Saving it unchanged keeps that deadline. Choose **Until unused for** to switch to
expiry after time without a connection instead.

## Forget a peer

Under **Peers**, click the trash can beside the peer and confirm **Forget Peer**.

This ends its connection immediately and removes that peer from the whole group. Other members remain trusted. The removed peer must pair again to rejoin. To stop sharing temporarily while retaining peers, use **Stop Sharing** instead.

Daisy retains removal history for up to 1,024 identities. When that history is full, existing members still work but new identities cannot join. Leaving the group and pairing again starts fresh; old removal history is discarded only when you leave.

The command `daisy forget <name-or-fingerprint>...` has the same group-wide effect. `daisy forget --all` instead makes this system leave the group; it removes no other members. See [command-line tasks](usage.md#optional-command-line).

## Accept systems at any time

Enable **Always discoverable** under **Advanced…** to accept new systems whenever sharing is on. Turning it on requires confirmation and Touch ID or the login password. While enabled, **Add a System** is unavailable. Turning it off restores invitation-based pairing.

An organization can [disable this setting](administration.md). See the [pairing security model](security-model.md#pairing) for acceptance windows, attempt limits and timeouts.

## Change trust or identity from the command line

The CLI can set a fixed term in days that connections do not extend. See [trust commands](usage.md#change-trust-from-the-command-line).

`daisy rotate-key` replaces both local identities and requires every peer to pair again. Unlock this system before rotating its device key. See [identity commands](usage.md#inspect-or-replace-identity) and [key protection](security-model.md#keys-at-rest).
