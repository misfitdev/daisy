# Virtual peer validation

The virtual peer fixture exercises Daisy's real in-memory Noise session,
device proof, pairing exchange, Once trust, and shared input receiver. Its
temporary identities and peer stores are discarded when the test exits. The
receiver uses a recording injector, so validation does not move the local
pointer or type into an application.

Run the scenario with:

```sh
just sparkle
tools/vpeer/run
```

The fixture lives in `mod.rs` and is imported only by the `share` unit tests.
Use `paired_channels()` from another unit test to start with two authenticated
and mutually paired virtual endpoints. Add event sequences to the scenario in
`src/share.rs`; failures stay reproducible and need no second system.
