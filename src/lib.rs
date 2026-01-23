#![allow(clippy::ptr_arg)]

pub mod local_config;
pub mod node_interface;
mod requests;
pub mod scanning;
pub mod transactions;
mod types;
pub mod wallet;

pub use local_config::*;
pub use node_interface::{IndexedHeight, IndexerStatus, NodeError, NodeInterface, Paged, Result};
pub use types::*;
pub use wallet::WalletStatus;

/// A Base58 encoded String of a Ergo P2PK address.
pub type P2PKAddressString = String;
/// A JSON String
pub type JsonString = String;
/// A JSON Value (using serde_json for consistency)
pub type JsonValue = serde_json::Value;
/// A Base58 encoded String of a Ergo P2S address.
pub type P2SAddressString = String;
/// The smallest unit of the Erg currency.
pub type NanoErg = u64;
/// A block height of the chain.
pub type BlockHeight = u64;
/// Duration in number of blocks.
pub type BlockDuration = u64;
/// A Base58 encoded String of a Token ID.
pub type TokenID = String;
