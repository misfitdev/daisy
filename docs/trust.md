# Manage trust

Pairing lets every member type and click on every other member's system. Members can also introduce new systems to the group. Only pair systems you trust with that access.

## Pair a system

For your first pair, follow [Getting started](getting-started.md). For an existing group, [add a system](usage.md#grow-a-group) through any member. The newcomer joins the whole group without another code.

Paired systems recognize each other using their network and Secure Enclave device keys. Copying Daisy's files to another system does not copy a usable device identity. See the [Security model](security-model.md#connections) for the authentication checks.

## Change how long trust lasts

Open Daisy and find the peer under **Peers**. Click its **Remember…** button, choose an option and click **Save**. The new duration starts from that moment.

| Choice | Trust ends when |
|---|---|
| Duration (4 days by default) | That many hours or days pass without a connection. Staying connected renews it. |
| End of session | The session ends. An accidental drop allows 60 seconds to reconnect; a deliberate stop does not. |
| Forever | You forget the peer. |

Each system keeps its own choice. Both must still trust each other to connect, so the stricter choice wins. When trust expires, pair again with a new code. Trust in an introduced system also ends when trust in its introducer ends.

## Forget a peer

Under **Peers**, click the trash can beside the peer and confirm **Forget Peer**.

This ends its connection immediately and removes it, along with systems it introduced, from the whole group. They must pair again to rejoin. To stop sharing temporarily while retaining peers, use **Stop Sharing** instead.

The command `daisy forget <name-or-fingerprint>...` has the same group-wide effect. `daisy forget --all` instead makes this system leave the group; it removes no other members. See [command-line tasks](usage.md#optional-command-line).

## Accept systems at any time

Enable **Always discoverable** under **Advanced…** to accept new systems whenever sharing is on. Turning it on requires confirmation and Touch ID or the login password. While enabled, **Add a System** is unavailable. Turning it off restores invitation-based pairing.

An organization can [disable this setting](administration.md). See the [pairing security model](security-model.md#pairing) for acceptance windows, attempt limits and timeouts.

## Change trust or identity from the command line

The CLI can set a fixed term in days that connections do not extend. See [trust commands](usage.md#change-trust-from-the-command-line).

`daisy rotate-key` replaces both local identities and requires every peer to pair again. Unlock this system before rotating its device key. See [identity commands](usage.md#inspect-or-replace-identity) and [key protection](security-model.md#keys-at-rest).
