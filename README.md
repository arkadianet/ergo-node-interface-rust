# Ergo Node Interface Library

A Rust library which makes interacting with and using an Ergo Node simple.

This crate uses the great [ergo-lib](https://github.com/ergoplatform/sigma-rust) (formerly sigma-rust) for parsing the `ErgoBox`es from the Ergo Node and other Ergo-related data-types.

Currently supported features include:
1. Core Ergo Node endpoints for writing off-chain dApps.
2. Helper functions on top of the supported endpoints which simplify the dApp developer experience.
3. A higher level interface for UTXO-set scanning.
4. ExtraIndex capability detection for nodes with enhanced blockchain queries.

100% of all Ergo Node endpoints are not supported in the present version, as the current goal is to focus on making the off-chain dApp developer experience as solid as possible. Full endpoint coverage is indeed a goal for the long-term nonetheless.

## Usage

Add to your `Cargo.toml`:
```toml
[dependencies]
ergo-node-interface = "0.6"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

### Basic Example
```rust
use ergo_node_interface::NodeInterface;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let node = NodeInterface::new("api_key", "127.0.0.1", "9053").await?;

    let height = node.current_block_height().await?;
    println!("Block height: {}", height);

    // Get box including mempool
    let utxo = node.box_from_id_with_pool("box_id_here").await?;

    Ok(())
}
```


Modules
========

The below are the currently implemented modules part of the Ergo Node Interface library.

Node Interface
--------------
This module contains the core `NodeInterface` struct which is used to interact with an Ergo Node. All endpoints are implemented as async methods on the `NodeInterface` struct.


```rust
let node = NodeInterface::new(api_key, ip, port).await?;
println!("Current height: {}", node.current_block_height().await?);
```

Furthermore a number of helper methods are implemented as well, such as:

```rust
/// Get the current state context of the blockchain
pub async fn get_state_context(&self) -> Result<ErgoStateContext>

/// Returns a sorted list of unspent boxes which cover at least the
/// provided value `total` of nanoErgs.
/// Note: This box selection strategy simply uses the largest
/// value holding boxes from the user's wallet first.
pub async fn unspent_boxes_with_min_total(&self, total: NanoErg) -> Result<Vec<ErgoBox>>
```

### ExtraIndex Awareness

The library automatically detects whether your node has extraIndex enabled:

```rust
// Check if node supports extraIndex endpoints
match node.has_extra_index() {
    Some(true) => {
        // Use powerful /blockchain/* queries
        let boxes = node.unspent_boxes_by_token_id(&token_id, 0, 100).await?;
    }
    Some(false) => {
        // ExtraIndex not available - use standard UTXO endpoints or scan registration
        println!("Consider using UTXO scan registration for tracking boxes");
    }
    None => {
        // Unknown - probe first, or use paged endpoints which auto-probe on 404
        node.refresh_capabilities().await;
        // Now check has_extra_index() again for a definitive answer
    }
}
```

Scanning
---------
This module contains the `Scan` struct which allows a developer to easily work with UTXO-set scans. Each `Scan` is tied to a specific `NodeInterface`, which is inspired from the fact that scans are saved on a per-node basis.

The `Scan` struct provides you with the ability to easily:
1. Register new scans with an Ergo Node.
2. Acquire boxes/serialized boxes from your registered scans.
3. Save/read scan ids to/from a local file.

Example using the scanning interface to register a scan to track an Oracle Pool:

```rust
let oracle_pool_nft_id = "08b59b14e4fdd60e5952314adbaa8b4e00bc0f0b676872a5224d3bf8591074cd".to_string();

let tracking_rule = object! {
        "predicate": "containsAsset",
        "assetId": oracle_pool_nft_id,
};

let scan = Scan::register(
    &"Oracle Pool Box Scan".to_string(),
    tracking_rule,
    node,
).await.unwrap();

```


Local Config
------------
This module provides a few helper functions to save/read from a local `node-interface.yaml` file which holds the Ergo Node ip/port/api key. This makes it much quicker for a dApp developer to get their dApp running without having to manually implement such logic himself.

Example functions which are available:

```rust
/// Create a new `node-interface.config` with the barebones yaml inside
pub fn create_new_local_config_file() -> Result<()>

/// Opens a local `node-interface.yaml` file and uses the
/// data inside to create a `NodeInterface` (sync, without probing)
pub fn new_interface_from_local_config() -> Result<NodeInterface>

/// Async version that probes for extraIndex capability
pub async fn new_interface_from_local_config_async() -> Result<NodeInterface>
```



Documentation
============

Documentation can be accessed via running the following command:

```rust
cargo doc --open
```


Contributing
============
See [CONTRIBUTING](CONTRIBUTING.md)
