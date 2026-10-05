# Swipe capture diagnostic

This separate app records numeric scroll and private gesture fields. It does
not start a Daisy session, change settings, inject input, or record keyboard
events or clipboard contents. Its event tap is listen-only. The callback records numeric samples in a fixed-capacity event buffer. Report
file writes happen outside the callback.

## Run the prepared app

1. Extract **Daisy Swipe Diagnostic.zip** into Downloads on the system being
   examined. Keep the app inside Downloads.
2. In Terminal, run:

   ```sh
   "$HOME/Downloads/Daisy Swipe Diagnostic.app/Contents/MacOS/daisy-swipe-diagnostic" "$HOME/Desktop/daisy-swipe-report.txt"
   ```

3. If it requests permission, enable **Daisy Swipe Diagnostic** in
   **System Settings → Privacy & Security → Input Monitoring** and
   **Accessibility**, then run the same command again. Use the **+** button
   to add the app if it does not appear.
4. Use this system's own trackpad and keep the pointer on its local display.
   Press Return for each direction. During its 12-second capture, repeat the
   requested three-finger swipe a few times, including a slow partial swipe.
   The four captures are **left**, **right**, **up**, and **down**.
5. Send **daisy-swipe-report.txt** from Desktop back for analysis. Mention which
   directions performed their normal action locally, and whether the swipes
   used three or four fingers.

Daisy can stay running. Afterward, remove the diagnostic's permission grants
and delete the diagnostic app when it is no longer needed.

## Build

```sh
mise exec -- cargo build --example swipe-diagnostic
```

The example calls only `macos::swipe::diagnose`; it avoids the normal Daisy
entry point, identity loading, network discovery and pointer restoration.
For distribution, place the executable at
`Daisy Swipe Diagnostic.app/Contents/MacOS/daisy-swipe-diagnostic`, install
the adjacent `Info.plist` at `Contents/Info.plist`, and sign the app with the
project's Developer ID identity. The prepared artifact targets Apple silicon.
