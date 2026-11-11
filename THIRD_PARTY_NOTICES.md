# Third-party notices

mcclient builds on the work of others. Thank you to everyone below.

## Horizon

Horizon is a from-scratch Rust implementation of Minecraft: Java Edition 26.3, by Ludovic Marie
and the Horizon contributors: <https://github.com/horizon-rust/horizon>.

mcclient is not a fork of Horizon and does not depend on it. Parts of it are ported from, or
modelled on, Horizon's `horizon-protocol` crate:

- packet layouts, and how they differ between protocol versions;
- the item stack and data component codecs;
- chunk section decoding;
- text component flattening;
- the reference behaviour of its own bot client.

Horizon's generated version adapters also supply the packet ids for protocol 775 (26.1).

Horizon is distributed under the following license:

```
MIT License

Copyright (c) 2026 Ludovic Marie and the Horizon contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## PrismarineJS minecraft-data

<https://github.com/PrismarineJS/minecraft-data> (MIT). It is the source of the 26.1 (protocol
775) item, entity, data component and block-state data, and of the `en_us` strings in
`src/lang.rs`. It was also used to cross-check the packet ids of 26.1 and 1.21.11.

## Minecraft Wiki

The [Java Edition protocol](https://minecraft.wiki/w/Java_Edition_protocol/Packets) documentation
on the Minecraft Wiki (formerly wiki.vg), by its many contributors. It was used as a reference for
packet structures and ids.

## Mojang / Minecraft

Block, item, entity and packet tables are generated from the reports of Minecraft's own data
generator (`net.minecraft.data.Main --reports`). Minecraft is a trademark of Mojang AB. mcclient
is not an official Minecraft product and is not approved by or associated with Mojang or
Microsoft.

## Rust crates

| Crate | License |
|---|---|
| [flate2](https://crates.io/crates/flate2) | MIT OR Apache-2.0 |
| [md-5](https://crates.io/crates/md-5) (RustCrypto) | MIT OR Apache-2.0 |
| [serde_json](https://crates.io/crates/serde_json) | MIT OR Apache-2.0 |

These crates' own dependencies are listed in `Cargo.lock`, and each carries its own license.
