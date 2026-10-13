# ancs-linux

Shows your iPhone's notifications on any freedesktop desktop over
Bluetooth. It uses Apple's
[Notification Center Service (ANCS)](https://developer.apple.com/library/archive/documentation/CoreBluetooth/Reference/AppleNotificationCenterServiceSpecification/),
the same protocol smartwatches use.

Notifications appear when they arrive on the iPhone, update in place when the iPhone
modifies them, and disappear when they are dismissed on both devices.

## Requirements

- Bluetooth LE advertising support (most have it), and BlueZ (5.66 or later).

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/schizophrenicautist/ancs-linux/main/install.sh | bash
```

Running the script downloads the latest release.

`./install.sh --uninstall` reverses it, keeping your config and cache. `--purge` removes those too.

## Pairing

1. Make the desktop pairable. e.g. `bluetoothctl pairable on`
2. With the daemon running, open Settings → Bluetooth on the iPhone. The desktop shows up
   under its Bluetooth name (the adapter alias, or `--name`). Connect to it.
3. Confirm that the PIN matches, on the iPhone and in KDE's dialog.
4. On later iOS versions you will be prompted to 'Allow *your desktop's name* to Receive Your iPhone Notifications' in a dialog. Press '**Allow**'.  
![alt text](https://github.com/schizophrenicautist/ancs-linux/blob/783c36d6654158a9fbf2db68bd7d7ad8a123148c/image/1.png "img1")
5. If the dialog doesn't appear, tap (i) next to the desktop and turn on **Share System Notifications**.
   The toggle can take a minute to appear, and only appears while the daemon is advertising.
   ![alt text](https://github.com/schizophrenicautist/ancs-linux/blob/783c36d6654158a9fbf2db68bd7d7ad8a123148c/image/2.png "img2")

## Build

```bash
make install
```

This builds a release binary into `~/.local/bin`, installs the systemd user unit
`ancs-linux.service` and starts it with the graphical session. `make uninstall` reverses it.

Building needs Rust and the D-Bus headers (`libdbus-1-dev` on Debian/Ubuntu, `dbus-devel` on
Fedora).

To run it by hand instead: `cargo run --release -- -v`.

## Configuration

Config lives in `~/.config/ancs-linux/config.toml`.

Apply changes with `systemctl --user restart ancs-linux`. 

Flags (`ancs-linux --help`) override the file.

## Troubleshooting

`journalctl --user -u ancs-linux -f` shows what the daemon is doing; add `-v` to `ExecStart`
for debug output.

| What went wrong | What to do |
|---|---|
| `could not register the ANCS advertisement` | `sudo systemctl restart bluetooth` |
| No "Share System Notifications" toggle, and the log says the iPhone is `connected over Classic Bluetooth only` | (KDE specific, but the process is similar on any DE) The iPhone was paired over Classic Bluetooth (from KDE's "Add device" wizard, or while the desktop was Classic-discoverable), so the bond has no LE keys. Forget the desktop on the iPhone, `bluetoothctl remove <iPhone address>`, keep KDE's wizard closed, and pair again by tapping the desktop in the iPhone's Bluetooth settings. **It's also possible that your adapter is broken or malfunctioning. Bluetooth LE has been part of the standard since Bluetooth 4.0.** |
| `cannot subscribe to Data Source yet` keeps repeating | Pairing has not finished, or **Share System Notifications** is off on the iPhone |
| The desktop never appears on the iPhone | Check the adapter supports LE advertising: `busctl --system introspect org.bluez /org/bluez/hci0 \| grep LEAdvertisingManager1` |
| Pairing fails, or the iPhone shows two entries for the desktop | Forget the desktop on the iPhone, `bluetoothctl remove <iPhone address>`, and pair again |
| The iPhone's audio moves to the desktop | Unrelated to this daemon; the iPhone treats a Classic Bluetooth bond as an audio device. Disconnect the audio profile in Plasma's Bluetooth settings |
