# mcclient

[![CI](https://github.com/MVRer/mcclient/actions/workflows/ci.yml/badge.svg)](https://github.com/MVRer/mcclient/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Minecraft 1.21.11 – 26.3](https://img.shields.io/badge/Minecraft-1.21.11%20%E2%80%93%2026.3-62B47A)
![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange)

**A lightweight, headless Minecraft: Java Edition client written in Rust, with a built-in
[MCP](https://modelcontextprotocol.io) server so AI agents can join a server and play.**

mcclient speaks the Minecraft network protocol directly. It needs no game installation, no Java
and no graphics, and ships as a single binary of under 2 MB. An AI agent drives it through tools:
chat, run commands, walk with path-finding, look around, use items and blocks, dig, interact with
entities and NPCs, read its inventory, and open and click chest-style menus. A simple
command-line mode is included for scripts.

```text
You:    Join the server, open /shop and buy the cheapest item.
Agent:  connect → command("shop") → menu → click_slot(13)
        "Bought 3× Emerald for 5 coins."
```

## Contents

- [Features](#features)
- [Supported versions](#supported-versions)
- [Installation](#installation)
- [Using it with an AI agent (MCP)](#using-it-with-an-ai-agent-mcp)
- [Using it from the command line](#using-it-from-the-command-line)
- [How it works](#how-it-works)
- [Limitations](#limitations)
- [Development](#development)
- [Credits](#credits)
- [License](#license)

## Features

- **The full modern join flow:** handshake, login, the configuration phase (known packs,
  registries, code of conduct, resource packs, cookies, plugin queries), compression, and proxy
  reconfiguration such as Velocity server switches.
- **A world model:** chunks are decoded into blocks for every supported version. The client
  tracks entities (players, mobs, NPCs, holograms), the tab list, health, food, game mode and
  dimension.
- **Movement:** a vanilla-like physics body with collisions, step-up, jumping and swimming, plus
  A* path-finding around obstacles.
- **Items and menus:** item stacks are decoded with their names, lore, enchantments and
  durability. Chest-style GUIs, like the ones plugins use, can be opened, read and clicked.
- **Interaction:** use items and blocks, place blocks, dig, attack, right-click entities, fill in
  signs, respawn.
- **Readable chat:** system, player, action bar, title and death messages are flattened to plain
  text, with translatable messages rendered in English.
- **Small and dependency-light:** three crates (`flate2`, `md-5`, `serde_json`) and no async
  runtime.

## Supported versions

| Minecraft | Protocol |
|---|---|
| **26.3** (default) | 777 |
| 26.2 | 776 |
| 26.1, 26.1.x | 775 |
| 1.21.11 | 774 |

The client pings the server and speaks the version it reports. Use `--protocol` to force one.

mcclient connects in **offline mode**: it doesn't sign in to a Minecraft account. The server, or
the proxy in front of it, must run with `online-mode=false`. That is the usual setup for local,
development and self-hosted servers.

## Installation

You need [Rust](https://www.rust-lang.org/tools/install) 1.88 or newer.

**Install from GitHub** (puts `mcclient` in `~/.cargo/bin`):

```bash
cargo install --git https://github.com/MVRer/mcclient
```

**Or build from a clone:**

```bash
git clone https://github.com/MVRer/mcclient
cd mcclient
cargo build --release
```

The binary is at `target/release/mcclient`. It runs on Linux, macOS and Windows.

Check the install:

```bash
mcclient --version
```

## Using it with an AI agent (MCP)

`mcclient mcp` runs an MCP server on stdin/stdout. It keeps one bot connected between tool calls
and connects on the first call, using the defaults you give it.

### Claude Code

```bash
claude mcp add mcclient -- mcclient mcp --server localhost:25565 --name Bot
```

If `mcclient` isn't on your `PATH`, use the binary's full path.

### Claude Desktop and other MCP clients

Add this to the client's MCP configuration. For Claude Desktop, that's
`claude_desktop_config.json`.

```json
{
  "mcpServers": {
    "mcclient": {
      "command": "mcclient",
      "args": ["mcp", "--server", "localhost:25565", "--name", "Bot"]
    }
  }
}
```

Then ask the agent in plain language, for example:

- *"Join the server and tell me who's online and what's around you."*
- *"Walk to the chest at 10 64 -3, open it and list its contents."*
- *"Talk to the villager named Shopkeeper and buy the first item in his menu."*

### Server options

| Option | Default | Description |
|---|---|---|
| `--server <host[:port]>` | — | Default server (the port defaults to 25565) |
| `--name <username>` | — | Default username: 1–16 characters, no spaces |
| `--view-distance <n>` | 6 | Chunks loaded around the bot (2–32) |
| `--protocol <n>` | auto | 774, 775, 776 or 777 |
| `--join-timeout <seconds>` | 30 | How long to wait to get into the game |
| `--lang <file>` | built-in | An `en_us.json` for full message translations |
| `-v`, `--verbose` | off | Log every packet to stderr |

### Tools

| Tool | Description |
|---|---|
| `connect`, `disconnect` | Join a server (optionally another host, name or protocol), or leave |
| `status` | Position, health, food, game mode, dimension, held item, open menu, movement state |
| `command` | Run a command (without the leading `/`); returns the chat that followed and any menu it opened |
| `chat` | Send a chat message |
| `read_chat`, `wait` | Read new chat lines, or wait until a line appears |
| `walk_to` | Path-find to a position (absolute or relative), optionally sprinting |
| `look`, `jump`, `sneak`, `stop` | Turn to angles or a point, jump, sneak, stop walking |
| `select_slot` | Choose the hotbar slot |
| `use_item` | Right-click the air with the held item |
| `use_block` | Right-click a block: open containers, press buttons, place blocks |
| `dig` | Break a block and wait until it's gone |
| `attack`, `interact` | Hit or right-click an entity, chosen by id or name |
| `inventory` | Inventory contents with item names, lore, enchantments and durability |
| `menu`, `click_slot`, `close_menu` | Read, click and close the open GUI |
| `drop_item`, `swap_hands`, `respawn` | Self-explanatory |
| `sign` | Write the lines of a sign editor the server opened |
| `entities` | Nearby entities, nearest first, with id, type, name, position and distance |
| `players` | The tab list, with game modes and ping |
| `block_at`, `find_blocks` | The block at a position, or blocks found by name nearby |

Action tools wait a moment (`wait_ms`) and report what happened: the new chat lines, and the
menu if one opened or changed. The agent can see the effect of each action without polling.

## Using it from the command line

One-shot mode joins, runs a list of commands, prints the chat it receives and leaves:

```bash
mcclient localhost:25565 Bot "help||list" 10
```

```text
mcclient <host[:port]> <username> "<cmd1>||<cmd2>||..." <seconds> [options]
```

- **Commands** are separated by `||` and written without the leading `/`.
- **`<seconds>`** is how long to stay after joining. The commands are sent at the start of that
  window.

| Option | Default | Description |
|---|---|---|
| `--protocol <n>` | auto | 774, 775, 776 or 777; skips the status ping |
| `--delay-ms <n>` | 500 | Pause between commands |
| `--join-timeout <seconds>` | 30 | Give up if not in the game by then |
| `--lang <file>` | built-in | An `en_us.json` for full message translations |
| `-v`, `--verbose` | off | Log every packet to stderr |

**stdout carries only the chat**, one line per message as plain text, which makes it easy to pipe
or grep:

```text
There are 1 of a max of 20 players online: Bot
<Alex> hello there
[actionbar] +5 coins
[title] Welcome!
```

Progress messages go to stderr.

| Exit code | Meaning |
|---|---|
| 0 | Joined and stayed for the requested time |
| 1 | Could not join. The message says why: refused, online-mode server, or the phase it timed out in |
| 2 | Invalid arguments |
| 3 | Joined, then got disconnected |

## How it works

| Module | Role |
|---|---|
| `engine` | One thread per connection ticking at 20 TPS like the real client (movement, tick end, chunk acknowledgements, keep-alives). It holds the game state and runs the agent's actions. |
| `physics` | A simplified vanilla player body with swept collisions, plus A* path-finding over the loaded chunks. The server validates every move, and the bot follows its corrections. |
| `world` | Per-version chunk decoding (paletted containers), names and properties for every block state, and approximate collision shapes (slabs, stairs, fences, doors, ...). |
| `item` | Item stacks with their data components. The useful ones are decoded; every other component is read past byte-exactly, so a single unknown component can't break a whole menu. |
| `mcp` | The MCP server: JSON-RPC 2.0 over stdio. |
| `client` | The one-shot command-line mode. |
| `ids`, `registries`, `lang`, `world/blocks_*` | Generated tables: packet ids, registries, block states and chat strings, per protocol. |

## Limitations

- **Offline mode only.** There is no Microsoft-account sign-in and no encryption, and chat and
  commands are sent unsigned.
- **Simplified physics.** There's no knockback simulation, elytra, vehicles, climbing or potion
  effects, and swimming is basic. Collision shapes are close to vanilla but not exact for every
  block. The server corrects any mismatch, and `walk_to` reports when it gets stuck.
- **Path-finding** only covers loaded chunks.
- **Special screens:** crafting, recipe books, anvils, enchanting tables, villager trades and
  books are not implemented. Their screens still open as menus and their slots can be clicked.
- **Entity names** come from custom names, the tab list and text displays. Other entity
  metadata isn't exposed.
- **Spam limit:** servers apply vanilla's command spam limit to non-operators.
- **SRV records** aren't resolved, so give the server's `host:port` directly.

## Development

```bash
cargo build
cargo test                                   # unit and end-to-end tests
cargo clippy --all-targets -- -D warnings
```

The end-to-end tests run the real binary against scripted mock servers:
- **CLI tests:** a full 26.3 session, a 26.1 proxy reconfiguration, a 1.21.11 login refusal and
  a stalled configuration phase.
- **MCP test:** an agent drives the bot over JSON-RPC through chunks, entities, an inventory, a
  shop menu with named items, and path-finding around a wall.

### Regenerating the data tables

The tables in `src/ids.rs`, `src/registries.rs`, `src/lang.rs` and `src/world/blocks_*.rs` are
generated by the scripts in [`tools/`](tools), which use only the Python 3 standard library.
Each script's header lists its exact arguments. The scripts read:

- Minecraft's data generator reports. Running the generator needs Java, but using mcclient
  doesn't. For each version:
  ```bash
  java -DbundlerMainClass=net.minecraft.data.Main -jar server.jar --reports
  ```
  The reports are written to `generated/reports/`.
- [PrismarineJS minecraft-data](https://github.com/PrismarineJS/minecraft-data), for 26.1.
- A checkout of [Horizon](https://github.com/horizon-rust/horizon), for its 26.1 version adapter.

## Credits

mcclient stands on the shoulders of:

- **[Horizon](https://github.com/horizon-rust/horizon)** by Ludovic Marie and the Horizon
  contributors (MIT). It's a Rust implementation of Minecraft 26.3, and the main reference for
  mcclient's packet layouts, version differences, item codecs and chunk decoding.
- **[PrismarineJS minecraft-data](https://github.com/PrismarineJS/minecraft-data)** (MIT), for
  26.1 data and English chat strings.
- **[The Minecraft Wiki protocol documentation](https://minecraft.wiki/w/Java_Edition_protocol/Packets)**
  (formerly wiki.vg) and its contributors.
- **Mojang**, for Minecraft's data generator, which produces the reports the tables are built from.

See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for details and license texts.

## License

[MIT](LICENSE).

*Not an official Minecraft product. Not approved by or associated with Mojang or Microsoft.
Minecraft is a trademark of Mojang AB.*
