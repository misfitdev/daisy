# Administration

Daisy reads settings that an administrator enforces with a configuration profile for the preference domain `dev.misfit.daisy`. It honors only enforced values, so a person cannot override them with `defaults write`.

| Key | Type | Effect |
|---|---|---|
| `AllowAlwaysDiscoverable` | Boolean | `false` keeps **Always discoverable** off. The switch in **Advanced…** explains that the organization turned it off. A system already set to always discoverable stops accepting new systems unless someone chooses **Add a System**, which still works. |

Without the key, people can turn on **Always discoverable**. Daisy asks for Touch ID or the login password before turning it on.

## Sample profile

Deploy this with any MDM that accepts custom configuration profiles. Replace both `PayloadUUID` values with UUIDs of your own (`uuidgen` prints one).

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>PayloadType</key>
  <string>Configuration</string>
  <key>PayloadVersion</key>
  <integer>1</integer>
  <key>PayloadIdentifier</key>
  <string>com.example.daisy</string>
  <key>PayloadUUID</key>
  <string>00000000-0000-0000-0000-000000000001</string>
  <key>PayloadDisplayName</key>
  <string>Daisy</string>
  <key>PayloadContent</key>
  <array>
    <dict>
      <key>PayloadType</key>
      <string>dev.misfit.daisy</string>
      <key>PayloadVersion</key>
      <integer>1</integer>
      <key>PayloadIdentifier</key>
      <string>com.example.daisy.settings</string>
      <key>PayloadUUID</key>
      <string>00000000-0000-0000-0000-000000000002</string>
      <key>AllowAlwaysDiscoverable</key>
      <false/>
    </dict>
  </array>
</dict>
</plist>
```

To check that a system received it:

```bash
defaults read "/Library/Managed Preferences/dev.misfit.daisy" AllowAlwaysDiscoverable
```

Daisy reads the setting when it opens, when sharing starts, and whenever someone tries to turn Always discoverable on. After installing or changing the profile, quit and reopen Daisy so a running session picks it up.
