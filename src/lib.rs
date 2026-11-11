//! mcclient: a lightweight headless Minecraft: Java Edition client that AI agents (over MCP) and
//! scripts can use to join a server and play.
//!
//! Joins in offline mode through the modern login flow (handshake, login, the 1.20.2+
//! configuration phase, play), sends chat commands with the unsigned `chat_command` packet and
//! prints the chat it receives. Speaks protocols 774-777 (1.21.11 to 26.3).

pub mod chat;
pub mod cli;
pub mod client;
pub mod engine;
pub mod ids;
pub mod item;
pub mod lang;
pub mod mcp;
pub mod nbt;
pub mod net;
pub mod physics;
pub mod registries;
pub mod text;
pub mod world;
