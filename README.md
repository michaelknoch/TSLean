# TSLean

A minimal, native TeamSpeak 3 client for macOS on Apple Silicon. Written in Rust, about 7 MB, near-zero idle CPU.

## Features

- Connect, browse the channel tree, switch channels
- Voice with Opus, voice activation with adjustable threshold, or global push-to-talk
- Channel, server and private chat with color emoji
- Per-user volume, join and leave sounds
- Light and dark mode

## Build

Requires Rust and `libopus` (`brew install opus`).

```sh
cargo build --release
./target/release/tslean
```

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.

Binaries statically include third-party code under their own permissive licenses (libopus BSD-3-Clause, egui's bundled fonts under OFL-1.1 and the Ubuntu Font License, Rust crates under MIT, Apache-2.0, BSD, ISC, MPL-2.0 and Zlib). Ship their notices with any binary release, e.g. generated with [cargo-about](https://github.com/EmbarkStudios/cargo-about).

## Credits

Built on [tsclientlib](https://github.com/ReSpeak/tsclientlib) by ReSpeak.

Unofficial project, not affiliated with or endorsed by TeamSpeak Systems GmbH. TeamSpeak is a trademark of its respective owner.
