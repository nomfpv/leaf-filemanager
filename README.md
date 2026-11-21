# leaf

leaf is a fast, keyboard-friendly dual-pane file manager written in Rust. It targets Windows 11 and Linux (KDE) with the same features on both, and it is built to open very large folders quickly while staying light on memory.

## Status

Early, private development. Windows works; Linux is in progress.

## Build

```
cargo build --release -p rf-app
cargo test -p rf-core -p rf-app
```

Requires a recent stable Rust toolchain.

## Credits

leaf is a fork of rustyfiles by Özer Güzel (MIT).

## Licence

MIT, see [LICENSE](LICENSE).

## Third-party licences

The licences of everything leaf is built on, and of the bundled icons, are listed in [THIRD-PARTY.md](THIRD-PARTY.md); regenerate it with `tools\third-party.ps1`.
