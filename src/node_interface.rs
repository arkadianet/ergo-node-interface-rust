//! The `NodeInterface` struct is defined which allows for interacting with an Ergo Node via Rust.

use crate::{BlockHeight, JsonValue, NanoErg, P2PKAddressString, P2SAddressString};
use ergo_lib::chain::ergo_state_context::{ErgoStateContext, Headers};
use ergo_lib::chain::parameters::Parameters;
use ergo_lib::ergo_chain_types::{Header, PreHeader};
use ergo_lib::ergotree_ir::chain::ergo_box::ErgoBox;
use ergo_lib::ergotree_ir::chain::token::TokenId;
use reqwest::Url;
use serde_json::from_str;
use std::convert::TryInto;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

/// Capability encoding for AtomicU8: 0=None, 1=Some(false), 2=Some(true)
const CAP_UNKNOWN: u8 = 0;
const CAP_FALSE: u8 = 1;
const CAP_TRUE: u8 = 2;

pub type Result<T> = std::result::Result<T, NodeError>;

#[derive(Error, Debug)]
pub enum NodeError {
    #[error("The configured node is unreachable. Please ensure your config is correctly filled out and the node is running.")]
    NodeUnreachable,
    #[error("Failed reading response from node: {0}")]
    FailedParsingNodeResponse(String),
    #[error("Failed parsing JSON box from node: {0}")]
    FailedParsingBox(String),
    #[error("No Boxes Were Found.")]
    NoBoxesFound,
    #[error("An insufficient number of Ergs were found.")]
    InsufficientErgsBalance(),
    #[error("Failed registering UTXO-set scan with the node: {0}")]
    FailedRegisteringScan(String),
    #[error("The node rejected the request you provided.\nNode Response: {0}")]
    BadRequest(String),
    #[error("The node wallet has no addresses.")]
    NoAddressesInWallet,
    #[error("The node is still syncing.")]
    NodeSyncing,
    #[error("Error while processing Node Interface Config Yaml: {0}")]
    YamlError(String),
    #[error("{0}")]
    Other(String),
    #[error("Failed parsing wallet status from node: {0}")]
    FailedParsingWalletStatus(String),
    #[error("Failed to parse URL: {0}")]
    InvalidUrl(String),
    #[error("Failed to parse scan ID: {0}")]
    InvalidScanId(String),
    #[error("This operation requires a node with extraIndex enabled. Configure the node with `extraIndex = true` or use a different endpoint.")]
    ExtraIndexRequired,
}

/// Wrapper for paged API responses that include items and total count.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Paged<T> {
    pub items: Vec<T>,
    pub total: u64,
}

/// The `NodeInterface` struct which holds the relevant Ergo node data
/// and has methods implemented to interact with the node.
#[derive(Debug, Clone)]
pub struct NodeInterface {
    pub api_key: String,
    pub url: Url,
    pub(crate) client: reqwest::Client,
    /// Tri-state with interior mutability: None = unknown, Some(true) = enabled, Some(false) = disabled
    /// Uses Arc<AtomicU8> so clones share learned capability state.
    has_extra_index: Arc<AtomicU8>,
}

pub fn is_mainnet_address(address: &str) -> bool {
    address.starts_with('9')
}

pub fn is_testnet_address(address: &str) -> bool {
    address.starts_with('3')
}

impl NodeInterface {
    fn capability_to_option(cap: u8) -> Option<bool> {
        match cap {
            CAP_TRUE => Some(true),
            CAP_FALSE => Some(false),
            _ => None,
        }
    }

    fn option_to_capability(opt: Option<bool>) -> u8 {
        match opt {
            Some(true) => CAP_TRUE,
            Some(false) => CAP_FALSE,
            None => CAP_UNKNOWN,
        }
    }

    fn create_client() -> Result<reqwest::Client> {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| NodeError::Other(format!("Failed to create HTTP client: {}", e)))
    }

    /// Create a new `NodeInterface` and probe for capabilities.
    ///
    /// This is an async constructor that connects to the node to detect
    /// if extraIndex is enabled. For sync contexts or to skip probing,
    /// use `new_without_probe()`.
    pub async fn new(api_key: &str, ip: &str, port: &str) -> Result<Self> {
        let url = Url::parse(&format!("http://{}:{}/", ip, port))
            .map_err(|e| NodeError::InvalidUrl(e.to_string()))?;
        Self::from_url_with_probe(api_key, url).await
    }

    /// Create from a URL string with capability detection.
    pub async fn from_url_str(api_key: &str, url: &str) -> Result<Self> {
        let url = Url::parse(url).map_err(|e| NodeError::InvalidUrl(e.to_string()))?;
        Self::from_url_with_probe(api_key, url).await
    }

    /// Create from a Url with capability detection.
    pub async fn from_url(api_key: &str, url: Url) -> Result<Self> {
        Self::from_url_with_probe(api_key, url).await
    }

    /// Create without probing for capabilities (sync-friendly).
    ///
    /// Capability is set to `None` (unknown), so extraIndex methods will
    /// be attempted and may fail with server 404 if not available.
    /// Call `refresh_capabilities()` later to probe.
    pub fn new_without_probe(api_key: &str, ip: &str, port: &str) -> Result<Self> {
        let url = Url::parse(&format!("http://{}:{}/", ip, port))
            .map_err(|e| NodeError::InvalidUrl(e.to_string()))?;
        Self::from_url_without_probe(api_key, url)
    }

    /// Create from Url without probing (sync-friendly).
    pub fn from_url_without_probe(api_key: &str, url: Url) -> Result<Self> {
        let client = Self::create_client()?;

        Ok(NodeInterface {
            api_key: api_key.to_string(),
            url,
            client,
            has_extra_index: Arc::new(AtomicU8::new(CAP_UNKNOWN)),
        })
    }

    async fn from_url_with_probe(api_key: &str, url: Url) -> Result<Self> {
        let client = Self::create_client()?;

        // Probe for extraIndex capability
        // Returns Some(true/false) if confirmed, None if unknown (network error, 5xx)
        let probed = Self::probe_extra_index(&client, &url, api_key).await;
        let cap = Self::option_to_capability(probed);

        Ok(NodeInterface {
            api_key: api_key.to_string(),
            url,
            client,
            has_extra_index: Arc::new(AtomicU8::new(cap)),
        })
    }

    /// Probe if node has extraIndex enabled by checking /blockchain/indexedHeight.
    /// Returns:
    /// - Some(true) if 200 OK
    /// - Some(false) if 404 (extraIndex confirmed disabled)
    /// - None on other errors (network failure, 5xx, etc.) - capability unknown
    async fn probe_extra_index(
        client: &reqwest::Client,
        base_url: &Url,
        api_key: &str,
    ) -> Option<bool> {
        let url = match base_url.join("/blockchain/indexedHeight") {
            Ok(u) => u,
            Err(_) => return None, // URL parse error - unknown
        };

        match client
            .get(url)
            .header("accept", "application/json")
            .header("api_key", api_key)
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    Some(true) // 200 OK - extraIndex enabled
                } else if resp.status() == reqwest::StatusCode::NOT_FOUND {
                    Some(false) // 404 - extraIndex disabled
                } else {
                    None // Other status (5xx, etc.) - unknown
                }
            }
            Err(_) => None, // Network error - unknown
        }
    }

    /// Check if this node has extraIndex enabled.
    ///
    /// Returns:
    /// - `Some(true)` - extraIndex confirmed enabled (from probe)
    /// - `Some(false)` - extraIndex confirmed disabled (from probe)
    /// - `None` - unknown (not yet probed; use `refresh_capabilities()` to probe)
    pub fn has_extra_index(&self) -> Option<bool> {
        Self::capability_to_option(self.has_extra_index.load(Ordering::Relaxed))
    }

    /// Re-probe the node for extraIndex capability.
    ///
    /// Updates capability to:
    /// - Some(true) on 200 OK
    /// - Some(false) on 404
    /// - Unchanged on network errors/5xx (keeps current value)
    ///
    /// Note: Takes &self (not &mut self) due to interior mutability.
    pub async fn refresh_capabilities(&self) {
        let probed = Self::probe_extra_index(&self.client, &self.url, &self.api_key).await;
        if let Some(result) = probed {
            // Only update if we got a definitive answer
            let cap = Self::option_to_capability(Some(result));
            self.has_extra_index.store(cap, Ordering::Relaxed);
        }
        // On None (network error), keep existing capability unchanged
    }

    /// Helper to check extraIndex and return error if KNOWN to be disabled.
    ///
    /// - `Some(true)` or `None` -> Ok (allow the call)
    /// - `Some(false)` -> Err(ExtraIndexRequired)
    fn require_extra_index(&self) -> Result<()> {
        if self.has_extra_index.load(Ordering::Relaxed) == CAP_FALSE {
            Err(NodeError::ExtraIndexRequired)
        } else {
            Ok(())
        }
    }

    /// Helper to handle 404 on paged extraIndex endpoints.
    ///
    /// Per swagger, 404 means "no results found". Returns:
    /// - `Ok(true)` if 404 and extraIndex confirmed enabled → caller should return empty Paged
    /// - `Ok(false)` if not 404 → caller should parse response normally
    /// - `Err(ExtraIndexRequired)` if 404 and extraIndex confirmed disabled
    /// - Probes if capability unknown, then applies above logic
    ///
    /// Note: Takes StatusCode (not &Response) to avoid holding reference across await,
    /// which would make the future !Send.
    async fn handle_paged_404(&self, status: reqwest::StatusCode) -> Result<bool> {
        if status != reqwest::StatusCode::NOT_FOUND {
            return Ok(false); // Not 404, parse normally
        }

        // 404 received - check capability
        match self.has_extra_index.load(Ordering::Relaxed) {
            CAP_TRUE => Ok(true), // Confirmed enabled, 404 = no results
            CAP_FALSE => Err(NodeError::ExtraIndexRequired),
            _ => {
                // Unknown - probe to find out
                self.refresh_capabilities().await;
                match self.has_extra_index.load(Ordering::Relaxed) {
                    CAP_TRUE => Ok(true),
                    CAP_FALSE => Err(NodeError::ExtraIndexRequired),
                    _ => {
                        // Still unknown after probe (network error) - surface error
                        Err(NodeError::FailedParsingNodeResponse(
                            "404 received but extraIndex capability unknown".to_string(),
                        ))
                    }
                }
            }
        }
    }

    // ==================== Standard Endpoints (no guard) ====================

    /// Get the current block height of the blockchain
    pub async fn current_block_height(&self) -> Result<BlockHeight> {
        let endpoint = "/info";
        let res = self.send_get_req(endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;

        let height_json = res_json["fullHeight"].clone();

        if height_json.is_null() {
            Err(NodeError::NodeSyncing)
        } else {
            height_json
                .to_string()
                .parse()
                .map_err(|_| NodeError::FailedParsingNodeResponse(res_json.to_string()))
        }
    }

    /// Get the full node info including blockchain parameters
    pub async fn node_info(&self) -> Result<JsonValue> {
        let endpoint = "/info";
        let res = self.send_get_req(endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;

        if res_json["fullHeight"].is_null() {
            Err(NodeError::NodeSyncing)
        } else {
            Ok(res_json)
        }
    }

    /// Get the current state context of the blockchain
    pub async fn get_state_context(&self) -> Result<ErgoStateContext> {
        let mut vec_headers = self.get_last_block_headers(10).await?;
        if vec_headers.len() < 10 {
            return Err(NodeError::Other(format!(
                "Expected 10 block headers, got {}",
                vec_headers.len()
            )));
        }
        vec_headers.reverse();
        let ten_headers: [Header; 10] = vec_headers
            .try_into()
            .map_err(|_| NodeError::Other("Failed to convert headers to array".to_string()))?;
        let headers = Headers::from(ten_headers);
        let pre_header = PreHeader::from(
            headers
                .first()
                .ok_or_else(|| NodeError::Other("Headers array is empty".to_string()))?
                .clone(),
        );
        let state_context = ErgoStateContext::new(pre_header, headers, Parameters::default());

        Ok(state_context)
    }

    /// Get the last `number` of block headers from the blockchain
    pub async fn get_last_block_headers(&self, number: u32) -> Result<Vec<Header>> {
        let endpoint = format!("/blocks/lastHeaders/{}", number);
        let res = self.send_get_req(endpoint.as_str()).await;
        let res_json = self.parse_response_to_json(res).await?;

        let mut headers: Vec<Header> = vec![];

        for i in 0.. {
            let header_json = &res_json[i];
            if header_json.is_null() {
                break;
            } else if let Ok(header) = from_str(&header_json.to_string()) {
                headers.push(header);
            }
        }
        Ok(headers)
    }

    /// Given a P2S Ergo address, extract the hex-encoded serialized ErgoTree (script)
    pub async fn p2s_to_tree(&self, address: &P2SAddressString) -> Result<String> {
        let endpoint = "/script/addressToTree/".to_string() + address;
        let res = self.send_get_req(&endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;

        Ok(res_json["tree"].to_string())
    }

    /// Given a P2S Ergo address, convert it to a hex-encoded Sigma byte array constant
    pub async fn p2s_to_bytes(&self, address: &P2SAddressString) -> Result<String> {
        let endpoint = "/script/addressToBytes/".to_string() + address;
        let res = self.send_get_req(&endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;

        Ok(res_json["bytes"].to_string())
    }

    /// Given a hex-encoded ErgoTree, convert it to an Ergo address
    pub async fn ergo_tree_to_address(&self, ergo_tree_hex: &String) -> Result<String> {
        let endpoint = "/utils/ergoTreeToAddress";
        let res = self.send_post_req(endpoint, ergo_tree_hex.clone()).await;
        let res_json = self.parse_response_to_json(res).await?;

        Ok(res_json["address"].to_string())
    }

    /// Given an Ergo P2PK Address, convert it to a raw hex-encoded EC point
    pub async fn p2pk_to_raw(&self, address: &P2PKAddressString) -> Result<String> {
        let endpoint = "/utils/addressToRaw/".to_string() + address;
        let res = self.send_get_req(&endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;

        Ok(res_json["raw"].to_string())
    }

    /// Given an Ergo P2PK Address, convert it to a raw hex-encoded EC point
    /// and prepend the type bytes so it is encoded and ready
    /// to be used in a register.
    pub async fn p2pk_to_raw_for_register(&self, address: &P2PKAddressString) -> Result<String> {
        let add = self.p2pk_to_raw(address).await?;
        Ok("07".to_string() + &add)
    }

    /// Given a raw hex-encoded EC point, convert it to a P2PK address
    pub async fn raw_to_p2pk(&self, raw: &str) -> Result<P2PKAddressString> {
        let endpoint = "/utils/rawToAddress/".to_string() + raw;
        let res = self.send_get_req(&endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;

        Ok(res_json["address"].to_string())
    }

    /// Given a raw hex-encoded EC point from a register (thus with type encoded characters in front),
    /// convert it to a P2PK address
    pub async fn raw_from_register_to_p2pk(&self, typed_raw: &str) -> Result<P2PKAddressString> {
        self.raw_to_p2pk(&typed_raw[2..]).await
    }

    /// Given a `Vec<ErgoBox>` return the given boxes (which must be part of the UTXO-set) as
    /// a vec of serialized strings in Base16 encoding
    pub async fn serialize_boxes(&self, b: &[ErgoBox]) -> Result<Vec<String>> {
        let mut results = Vec::with_capacity(b.len());
        for ergo_box in b {
            // Preserve best-effort: empty string on failure (matches current behavior)
            let serialized = self
                .serialized_box_from_id(&ergo_box.box_id().into())
                .await
                .unwrap_or_else(|_| "".to_string());
            results.push(serialized);
        }
        Ok(results)
    }

    /// Given an `ErgoBox` return the given box (which must be part of the UTXO-set) as
    /// a serialized string in Base16 encoding
    pub async fn serialize_box(&self, b: &ErgoBox) -> Result<String> {
        self.serialized_box_from_id(&b.box_id().into()).await
    }

    /// Given a box id return the given box (which must be part of the
    /// UTXO-set) as a serialized string in Base16 encoding
    pub async fn serialized_box_from_id(&self, box_id: &String) -> Result<String> {
        let endpoint = "/utxo/byIdBinary/".to_string() + box_id;
        let res = self.send_get_req(&endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;

        Ok(res_json["bytes"].to_string())
    }

    /// Given a box id return the given box (which must be part of the UTXO-set)
    pub async fn box_from_id(&self, box_id: &String) -> Result<ErgoBox> {
        let endpoint = "/utxo/byId/".to_string() + box_id;
        let res = self.send_get_req(&endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;

        if let Ok(ergo_box) = from_str(&res_json.to_string()) {
            Ok(ergo_box)
        } else {
            Err(NodeError::FailedParsingBox(
                serde_json::to_string_pretty(&res_json).unwrap_or_else(|_| res_json.to_string()),
            ))
        }
    }

    /// Get box by ID including mempool (unconfirmed transactions).
    ///
    /// Unlike `box_from_id()`, this also checks the mempool for unconfirmed boxes.
    pub async fn box_from_id_with_pool(&self, box_id: impl AsRef<str>) -> Result<ErgoBox> {
        let endpoint = format!("/utxo/withPool/byId/{}", box_id.as_ref());
        let res = self.send_get_req(&endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;
        from_str(&res_json.to_string())
            .map_err(|_| NodeError::FailedParsingBox(res_json.to_string()))
    }

    /// Get multiple boxes by IDs including mempool.
    /// Strict parsing: errors on first box parse failure.
    pub async fn boxes_from_ids_with_pool(&self, box_ids: &[String]) -> Result<Vec<ErgoBox>> {
        let endpoint = "/utxo/withPool/byIds";
        let body =
            serde_json::to_string(box_ids).map_err(|e| NodeError::Other(e.to_string()))?;
        let res = self.send_post_req(endpoint, body).await;
        let res_json = self.parse_response_to_json(res).await?;

        let arr = res_json
            .as_array()
            .ok_or_else(|| NodeError::FailedParsingNodeResponse(res_json.to_string()))?;

        let mut boxes = Vec::with_capacity(arr.len());
        for item in arr {
            let ergo_box: ErgoBox = from_str(&item.to_string())
                .map_err(|_| NodeError::FailedParsingBox(item.to_string()))?;
            boxes.push(ergo_box);
        }
        Ok(boxes)
    }

    /// Get serialized box by ID including mempool.
    pub async fn serialized_box_from_id_with_pool(&self, box_id: impl AsRef<str>) -> Result<String> {
        let endpoint = format!("/utxo/withPool/byIdBinary/{}", box_id.as_ref());
        let res = self.send_get_req(&endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;
        // Match existing serialized_box_from_id behavior: .to_string() (includes quotes)
        Ok(res_json["bytes"].to_string())
    }

    /// Get block header IDs at the specified height.
    ///
    /// Returns a vector of block IDs (there may be multiple due to forks).
    pub async fn block_ids_at_height(&self, height: u64) -> Result<Vec<String>> {
        let endpoint = format!("/blocks/at/{}", height);
        let res = self.send_get_req(&endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;
        serde_json::from_value(res_json.clone())
            .map_err(|e| NodeError::FailedParsingNodeResponse(e.to_string()))
    }

    /// Get block by header ID
    pub async fn get_block(&self, header_id: impl AsRef<str>) -> Result<JsonValue> {
        let endpoint = format!("/blocks/{}", header_id.as_ref());
        let res = self.send_get_req(&endpoint).await;
        self.parse_response_to_json(res).await
    }

    /// Get block header by ID
    pub async fn get_block_header(&self, header_id: impl AsRef<str>) -> Result<JsonValue> {
        let endpoint = format!("/blocks/{}/header", header_id.as_ref());
        let res = self.send_get_req(&endpoint).await;
        self.parse_response_to_json(res).await
    }

    /// Get block transactions by header ID
    pub async fn get_block_transactions(&self, header_id: impl AsRef<str>) -> Result<JsonValue> {
        let endpoint = format!("/blocks/{}/transactions", header_id.as_ref());
        let res = self.send_get_req(&endpoint).await;
        self.parse_response_to_json(res).await
    }

    // ==================== ExtraIndex Endpoints (with guard) ====================

    /// Checks if the blockchain indexer is active by querying the node.
    /// NOTE: This method is intentionally NOT guarded to preserve current behavior.
    /// Returns inactive status on non-extraIndex nodes (including 404).
    pub async fn indexer_status(&self) -> Result<IndexerStatus> {
        let endpoint = "/blockchain/indexedHeight";
        let res = self.send_get_req(endpoint).await?;

        // Handle 404 (extraIndex not enabled) before attempting JSON parse
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(IndexerStatus {
                is_active: false,
                is_sync: false,
            });
        }

        let res_json = self.parse_response_to_json(Ok(res)).await?;

        let error = res_json["error"].clone();
        if !error.is_null() {
            return Ok(IndexerStatus {
                is_active: false,
                is_sync: false,
            });
        }

        let full_height = res_json["fullHeight"]
            .as_u64()
            .ok_or(NodeError::FailedParsingNodeResponse(res_json.to_string()))?;
        let indexed_height = res_json["indexedHeight"]
            .as_u64()
            .ok_or(NodeError::FailedParsingNodeResponse(res_json.to_string()))?;

        let is_sync = full_height.abs_diff(indexed_height) < 10;
        Ok(IndexerStatus {
            is_active: true,
            is_sync,
        })
    }

    /// Get the current indexed height (extraIndex nodes only).
    ///
    /// **Requires:** Node must have `extraIndex = true`.
    ///
    /// Useful for checking sync status of the extra index.
    pub async fn get_indexed_height(&self) -> Result<IndexedHeight> {
        self.require_extra_index()?;
        let endpoint = "/blockchain/indexedHeight";
        let res = self.send_get_req(endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;
        Ok(IndexedHeight {
            indexed_height: res_json["indexedHeight"]
                .as_u64()
                .ok_or_else(|| NodeError::FailedParsingNodeResponse(res_json.to_string()))?
                as u32,
            full_height: res_json["fullHeight"]
                .as_u64()
                .ok_or_else(|| NodeError::FailedParsingNodeResponse(res_json.to_string()))?
                as u32,
        })
    }

    /// Acquires unspent boxes from the blockchain by specific address
    ///
    /// **Requires:** Node must have `extraIndex = true`.
    ///
    /// Note: The Ergo node's unspent endpoints do not provide a total count,
    /// so pagination must be done by requesting pages until fewer than `limit`
    /// items are returned.
    pub async fn unspent_boxes_by_address(
        &self,
        address: &P2PKAddressString,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<ErgoBox>> {
        self.require_extra_index()?;
        let endpoint = format!(
            "/blockchain/box/unspent/byAddress?offset={}&limit={}",
            offset, limit
        );
        let res = self.send_post_req(endpoint.as_str(), address.clone()).await;
        let res_json = self.parse_response_to_json(res).await?;

        let mut box_list = vec![];

        for i in 0.. {
            let box_json = &res_json[i];
            if box_json.is_null() {
                break;
            } else if let Ok(ergo_box) = from_str(&box_json.to_string()) {
                // This condition is added due to a bug in the node indexer that returns some spent boxes as unspent.
                if box_json["spentTransactionId"].is_null() {
                    box_list.push(ergo_box);
                }
            }
        }
        Ok(box_list)
    }

    /// Acquires unspent boxes from the blockchain by specific token_id
    ///
    /// **Requires:** Node must have `extraIndex = true`.
    ///
    /// Note: The Ergo node's unspent endpoints do not provide a total count,
    /// so pagination must be done by requesting pages until fewer than `limit`
    /// items are returned.
    pub async fn unspent_boxes_by_token_id(
        &self,
        token_id: &TokenId,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<ErgoBox>> {
        self.require_extra_index()?;
        let id: String = (*token_id).into();
        let endpoint = format!(
            "/blockchain/box/unspent/byTokenId/{}?offset={}&limit={}",
            id, offset, limit
        );
        let res = self.send_get_req(endpoint.as_str()).await;
        let res_json = self.parse_response_to_json(res).await?;

        let mut box_list = vec![];

        for i in 0.. {
            let box_json = &res_json[i];
            if box_json.is_null() {
                break;
            } else if let Ok(ergo_box) = from_str(&box_json.to_string()) {
                // This condition is added due to a bug in the node indexer that returns some spent boxes as unspent.
                if box_json["spentTransactionId"].is_null() {
                    box_list.push(ergo_box);
                }
            }
        }
        Ok(box_list)
    }

    /// Get the current nanoErgs balance held in the `address`
    ///
    /// **Requires:** Node must have `extraIndex = true`.
    pub async fn nano_ergs_balance(&self, address: &P2PKAddressString) -> Result<NanoErg> {
        self.require_extra_index()?;
        let endpoint = "/blockchain/balance";
        let res = self.send_post_req(endpoint, address.clone()).await;
        let res_json = self.parse_response_to_json(res).await?;

        let balance = res_json["confirmed"]["nanoErgs"].clone();

        if balance.is_null() {
            Err(NodeError::NodeSyncing)
        } else {
            balance
                .as_u64()
                .ok_or_else(|| NodeError::FailedParsingNodeResponse(res_json.to_string()))
        }
    }

    /// Given a box id return the given box from the blockchain
    ///
    /// **Requires:** Node must have `extraIndex = true`.
    pub async fn blockchain_box_from_id(&self, box_id: &String) -> Result<ErgoBox> {
        self.require_extra_index()?;
        let endpoint = "/blockchain/box/byId/".to_string() + box_id;
        let res = self.send_get_req(&endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;

        if let Ok(ergo_box) = from_str(&res_json.to_string()) {
            Ok(ergo_box)
        } else {
            Err(NodeError::FailedParsingBox(
                serde_json::to_string_pretty(&res_json).unwrap_or_else(|_| res_json.to_string()),
            ))
        }
    }

    /// Given a transaction id return the given transaction from the blockchain
    ///
    /// **Requires:** Node must have `extraIndex = true`.
    pub async fn blockchain_transaction_from_id(&self, tx_id: &String) -> Result<JsonValue> {
        self.require_extra_index()?;
        let endpoint = "/blockchain/transaction/byId/".to_string() + tx_id;
        let res = self.send_get_req(&endpoint).await;
        self.parse_response_to_json(res).await
    }

    /// Get token metadata from the blockchain
    ///
    /// **Requires:** Node must have `extraIndex = true`.
    pub async fn get_token_info(&self, token_id: &str) -> Result<JsonValue> {
        self.require_extra_index()?;
        let endpoint = format!("/blockchain/token/byId/{}", token_id);
        let res = self.send_get_req(&endpoint).await;
        self.parse_response_to_json(res).await
    }

    /// Acquires unspent boxes from the blockchain by ErgoTree
    ///
    /// **Requires:** Node must have `extraIndex = true`.
    ///
    /// Note: Filters out boxes with non-null `spentTransactionId` due to a known
    /// node indexer bug that can return spent boxes.
    ///
    /// Note: The Ergo node's unspent endpoints do not provide a total count,
    /// so pagination must be done by requesting pages until fewer than `limit`
    /// items are returned.
    pub async fn unspent_boxes_by_ergo_tree(
        &self,
        ergo_tree: &str,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<ErgoBox>> {
        self.require_extra_index()?;
        let endpoint = format!(
            "/blockchain/box/unspent/byErgoTree?offset={}&limit={}",
            offset, limit
        );
        let res = self.send_post_req(&endpoint, ergo_tree.to_string()).await;
        let res_json = self.parse_response_to_json(res).await?;

        let mut box_list = vec![];

        for i in 0.. {
            let box_json = &res_json[i];
            if box_json.is_null() {
                break;
            } else if let Ok(ergo_box) = from_str(&box_json.to_string()) {
                // Filter out spent boxes due to node indexer bug that returns some spent boxes as unspent
                if box_json["spentTransactionId"].is_null() {
                    box_list.push(ergo_box);
                }
            }
        }
        Ok(box_list)
    }

    /// Get boxes by address (including spent). Requires extraIndex.
    /// Returns paged results with total count.
    /// Returns empty Paged on 404 (no results) when extraIndex is enabled.
    pub async fn boxes_by_address(
        &self,
        address: &P2PKAddressString,
        offset: u64,
        limit: u64,
    ) -> Result<Paged<ErgoBox>> {
        self.require_extra_index()?;
        let endpoint = format!("/blockchain/box/byAddress?offset={}&limit={}", offset, limit);
        let response = self.send_post_req(&endpoint, address.clone()).await?;

        // Capture status before consuming response (avoids !Send issue)
        let status = response.status();
        if self.handle_paged_404(status).await? {
            return Ok(Paged {
                items: vec![],
                total: 0,
            });
        }

        let text = response.text().await.map_err(|_| {
            NodeError::FailedParsingNodeResponse("Response not parseable into text".to_string())
        })?;
        let res_json: JsonValue = serde_json::from_str(&text)
            .map_err(|_| NodeError::FailedParsingNodeResponse(text.clone()))?;

        let total = res_json["total"].as_u64().ok_or_else(|| {
            NodeError::FailedParsingNodeResponse(format!("Missing 'total' field: {}", res_json))
        })?;
        let items_arr = res_json["items"].as_array().ok_or_else(|| {
            NodeError::FailedParsingNodeResponse(format!("Missing 'items' array: {}", res_json))
        })?;

        let mut items = Vec::with_capacity(items_arr.len());
        for item in items_arr {
            let ergo_box: ErgoBox = from_str(&item.to_string())
                .map_err(|_| NodeError::FailedParsingBox(item.to_string()))?;
            items.push(ergo_box);
        }

        Ok(Paged { items, total })
    }

    /// Get boxes by token ID (including spent). Requires extraIndex.
    /// Returns paged results with total count.
    /// Returns empty Paged on 404 (no results) when extraIndex is enabled.
    pub async fn boxes_by_token_id(
        &self,
        token_id: &TokenId,
        offset: u64,
        limit: u64,
    ) -> Result<Paged<ErgoBox>> {
        self.require_extra_index()?;
        let id: String = (*token_id).into();
        let endpoint = format!(
            "/blockchain/box/byTokenId/{}?offset={}&limit={}",
            id, offset, limit
        );
        let response = self.send_get_req(&endpoint).await?;

        // Capture status before consuming response (avoids !Send issue)
        let status = response.status();
        if self.handle_paged_404(status).await? {
            return Ok(Paged {
                items: vec![],
                total: 0,
            });
        }

        let text = response.text().await.map_err(|_| {
            NodeError::FailedParsingNodeResponse("Response not parseable into text".to_string())
        })?;
        let res_json: JsonValue = serde_json::from_str(&text)
            .map_err(|_| NodeError::FailedParsingNodeResponse(text.clone()))?;

        let total = res_json["total"].as_u64().ok_or_else(|| {
            NodeError::FailedParsingNodeResponse(format!("Missing 'total' field: {}", res_json))
        })?;
        let items_arr = res_json["items"].as_array().ok_or_else(|| {
            NodeError::FailedParsingNodeResponse(format!("Missing 'items' array: {}", res_json))
        })?;

        let mut items = Vec::with_capacity(items_arr.len());
        for item in items_arr {
            let ergo_box: ErgoBox = from_str(&item.to_string())
                .map_err(|_| NodeError::FailedParsingBox(item.to_string()))?;
            items.push(ergo_box);
        }

        Ok(Paged { items, total })
    }

    /// Get transactions by address. Requires extraIndex.
    /// Returns paged results with total count.
    /// Returns empty Paged on 404 (no results) when extraIndex is enabled.
    pub async fn transactions_by_address(
        &self,
        address: &P2PKAddressString,
        offset: u64,
        limit: u64,
    ) -> Result<Paged<JsonValue>> {
        self.require_extra_index()?;
        let endpoint = format!(
            "/blockchain/transaction/byAddress?offset={}&limit={}",
            offset, limit
        );
        let response = self.send_post_req(&endpoint, address.clone()).await?;

        // Capture status before consuming response (avoids !Send issue)
        let status = response.status();
        if self.handle_paged_404(status).await? {
            return Ok(Paged {
                items: vec![],
                total: 0,
            });
        }

        let text = response.text().await.map_err(|_| {
            NodeError::FailedParsingNodeResponse("Response not parseable into text".to_string())
        })?;
        let res_json: JsonValue = serde_json::from_str(&text)
            .map_err(|_| NodeError::FailedParsingNodeResponse(text.clone()))?;

        let total = res_json["total"].as_u64().ok_or_else(|| {
            NodeError::FailedParsingNodeResponse(format!("Missing 'total' field: {}", res_json))
        })?;
        let items = res_json["items"]
            .as_array()
            .ok_or_else(|| {
                NodeError::FailedParsingNodeResponse(format!(
                    "Missing 'items' array: {}",
                    res_json
                ))
            })?
            .to_vec();

        Ok(Paged { items, total })
    }
}

/// Status of the blockchain indexer active/sync state
pub struct IndexerStatus {
    pub is_active: bool,
    pub is_sync: bool,
}

/// Status of the blockchain indexer height
#[derive(Debug, Clone)]
pub struct IndexedHeight {
    pub indexed_height: u32,
    pub full_height: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ergo_lib::ergo_chain_types::Digest32;
    use std::convert::TryFrom;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Valid test fixtures - boxId must match the hash of the box content
    // Box 1: 1 ERG, height 100, txId all zeros, index 0
    const VALID_BOX_ID: &str =
        "7d3311db4b8efeaae1773ec98fc490cb1c32f0b791581fc551d4d160ae6133c7";
    // Box 2: 0.5 ERG, height 50, txId all ones, index 0 (used for spent box test)
    const VALID_BOX_ID_2: &str =
        "153979dd5e97b776d30fbd5daab57aa9708daeeed0534833955e1d8e838095b6";
    // Box 3: 1 ERG with token, height 100, txId all zeros, index 0
    const VALID_BOX_ID_WITH_TOKEN: &str =
        "e7f0dd52b5aa171ba119ce56c5ce74f89dd14b7957440e173b5d7639db6ea89a";
    const VALID_ERGO_TREE: &str =
        "0008cd0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
    const VALID_HEADER_ID: &str =
        "b0244dfc267baca974a4caee06120321562784303a8a688976ae56170e4d175b";
    const VALID_TX_ID: &str =
        "0000000000000000000000000000000000000000000000000000000000000000";
    const VALID_TX_ID_2: &str =
        "1111111111111111111111111111111111111111111111111111111111111111";

    #[tokio::test]
    async fn test_capability_probe_extraindex_enabled() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "indexedHeight": 1000,
                "fullHeight": 1000
            })))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_str("", &mock_server.uri())
            .await
            .unwrap();
        assert_eq!(node.has_extra_index(), Some(true));
    }

    #[tokio::test]
    async fn test_capability_probe_extraindex_disabled() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_str("", &mock_server.uri())
            .await
            .unwrap();
        assert_eq!(node.has_extra_index(), Some(false));
    }

    #[tokio::test]
    async fn test_guard_blocks_when_extraindex_disabled() {
        let mock_server = MockServer::start().await;

        // Probe returns 404 -> extraIndex disabled
        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_str("", &mock_server.uri())
            .await
            .unwrap();

        // Use valid 64-hex token ID
        let digest: Digest32 = Digest32::try_from(
            "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        )
        .unwrap();
        let token_id: TokenId = TokenId::from(digest);

        // Guarded method should return ExtraIndexRequired immediately (before network call)
        let result = node.unspent_boxes_by_token_id(&token_id, 0, 10).await;
        assert!(matches!(result, Err(NodeError::ExtraIndexRequired)));
    }

    #[tokio::test]
    async fn test_paged_404_returns_empty_when_extraindex_enabled() {
        // When extraIndex is confirmed enabled, 404 on paged endpoints = empty results
        let mock_server = MockServer::start().await;

        // Probe returns 200 -> extraIndex enabled
        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "indexedHeight": 1000, "fullHeight": 1000
                })),
            )
            .mount(&mock_server)
            .await;

        // Search endpoint returns 404 (no results found)
        Mock::given(method("POST"))
            .and(path("/blockchain/box/byAddress"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_str("", &mock_server.uri())
            .await
            .unwrap();
        assert_eq!(node.has_extra_index(), Some(true));

        // 404 should return empty Paged, not an error
        let result = node
            .boxes_by_address(&"address".to_string(), 0, 10)
            .await
            .unwrap();
        assert_eq!(result.items.len(), 0);
        assert_eq!(result.total, 0);
    }

    #[tokio::test]
    async fn test_paged_404_probes_when_capability_unknown() {
        // When capability unknown, 404 triggers a probe, then returns empty if enabled
        let mock_server = MockServer::start().await;

        let node = NodeInterface::from_url_without_probe(
            "",
            reqwest::Url::parse(&mock_server.uri()).unwrap(),
        )
        .unwrap();
        assert_eq!(node.has_extra_index(), None);

        // Probe will return 200 -> extraIndex enabled
        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "indexedHeight": 1000, "fullHeight": 1000
                })),
            )
            .mount(&mock_server)
            .await;

        // Search endpoint returns 404
        Mock::given(method("POST"))
            .and(path("/blockchain/box/byAddress"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        // Should probe, learn extraIndex=true, return empty Paged
        let result = node
            .boxes_by_address(&"address".to_string(), 0, 10)
            .await
            .unwrap();
        assert_eq!(result.items.len(), 0);
        assert_eq!(result.total, 0);
        assert_eq!(node.has_extra_index(), Some(true)); // Capability now learned
    }

    #[tokio::test]
    async fn test_paged_404_errors_when_extraindex_disabled() {
        // When extraIndex is confirmed disabled, 404 should return ExtraIndexRequired
        let mock_server = MockServer::start().await;

        // Probe returns 404 -> extraIndex disabled
        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_str("", &mock_server.uri())
            .await
            .unwrap();
        assert_eq!(node.has_extra_index(), Some(false));

        // Guard should block the call before even hitting the endpoint
        let result = node.boxes_by_address(&"address".to_string(), 0, 10).await;
        assert!(matches!(result, Err(NodeError::ExtraIndexRequired)));
    }

    #[tokio::test]
    async fn test_capability_shared_across_clones_via_refresh() {
        let mock_server = MockServer::start().await;

        // First request gets 404 (extraIndex disabled)
        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let node1 = NodeInterface::from_url_str("", &mock_server.uri())
            .await
            .unwrap();
        let node2 = node1.clone(); // Clones share Arc<AtomicU8>

        // Both should see capability as Some(false) from probe
        assert_eq!(node1.has_extra_index(), Some(false));
        assert_eq!(node2.has_extra_index(), Some(false));
    }

    #[tokio::test]
    async fn test_standard_endpoint_works_without_extraindex() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "fullHeight": 12345,
                "name": "ergo-mainnet"
            })))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_without_probe(
            "",
            reqwest::Url::parse(&mock_server.uri()).unwrap(),
        )
        .unwrap();

        let height = node.current_block_height().await.unwrap();
        assert_eq!(height, 12345);
    }

    #[tokio::test]
    async fn test_required_headers_sent() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/info"))
            .and(header("accept", "application/json"))
            .and(header("api_key", "test_key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "fullHeight": 100
            })))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_without_probe(
            "test_key",
            reqwest::Url::parse(&mock_server.uri()).unwrap(),
        )
        .unwrap();

        // This will fail if headers aren't sent correctly
        let height = node.current_block_height().await.unwrap();
        assert_eq!(height, 100);
    }

    #[tokio::test]
    async fn test_parse_indexed_height_response() {
        let mock_server = MockServer::start().await;

        // First mock the probe
        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "indexedHeight": 950000,
                    "fullHeight": 950100
                })),
            )
            .expect(2) // Once for probe, once for actual call
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_str("", &mock_server.uri())
            .await
            .unwrap();
        let indexed = node.get_indexed_height().await.unwrap();

        assert_eq!(indexed.indexed_height, 950000);
        assert_eq!(indexed.full_height, 950100);
    }

    #[tokio::test]
    async fn test_parse_block_ids_at_height() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/blocks/at/100"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!(["block_header_id_1", "block_header_id_2"])),
            )
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_without_probe(
            "",
            reqwest::Url::parse(&mock_server.uri()).unwrap(),
        )
        .unwrap();

        let ids = node.block_ids_at_height(100).await.unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], "block_header_id_1");
    }

    #[tokio::test]
    async fn test_get_block() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path(format!("/blocks/{}", VALID_HEADER_ID)))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "header": { "id": VALID_HEADER_ID, "height": 100 },
                "blockTransactions": { "headerId": VALID_HEADER_ID, "transactions": [] }
            })))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_without_probe(
            "",
            reqwest::Url::parse(&mock_server.uri()).unwrap(),
        )
        .unwrap();

        let block = node.get_block(VALID_HEADER_ID).await.unwrap();
        assert_eq!(block["header"]["id"], VALID_HEADER_ID);
    }

    #[tokio::test]
    async fn test_get_block_header() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path(format!("/blocks/{}/header", VALID_HEADER_ID)))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": VALID_HEADER_ID,
                "height": 100,
                "timestamp": 1234567890u64
            })))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_without_probe(
            "",
            reqwest::Url::parse(&mock_server.uri()).unwrap(),
        )
        .unwrap();

        let header = node.get_block_header(VALID_HEADER_ID).await.unwrap();
        assert_eq!(header["height"], 100);
    }

    #[tokio::test]
    async fn test_get_block_transactions() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path(format!("/blocks/{}/transactions", VALID_HEADER_ID)))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "headerId": VALID_HEADER_ID,
                "transactions": []
            })))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_without_probe(
            "",
            reqwest::Url::parse(&mock_server.uri()).unwrap(),
        )
        .unwrap();

        let txs = node.get_block_transactions(VALID_HEADER_ID).await.unwrap();
        assert_eq!(txs["headerId"], VALID_HEADER_ID);
    }

    #[tokio::test]
    async fn test_boxes_from_ids_with_pool() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/utxo/withPool/byIds"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "boxId": VALID_BOX_ID,
                    "value": 1000000000u64,
                    "ergoTree": VALID_ERGO_TREE,
                    "creationHeight": 100,
                    "assets": [],
                    "additionalRegisters": {},
                    "transactionId": VALID_TX_ID,
                    "index": 0
                }
            ])))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_without_probe(
            "",
            reqwest::Url::parse(&mock_server.uri()).unwrap(),
        )
        .unwrap();

        let boxes = node
            .boxes_from_ids_with_pool(&[VALID_BOX_ID.to_string()])
            .await
            .unwrap();
        assert_eq!(boxes.len(), 1);
    }

    #[tokio::test]
    async fn test_paged_boxes_by_address() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "indexedHeight": 1000, "fullHeight": 1000
                })),
            )
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/blockchain/box/byAddress"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{
                    "boxId": VALID_BOX_ID,
                    "value": 1000000000u64,
                    "ergoTree": VALID_ERGO_TREE,
                    "creationHeight": 100,
                    "assets": [],
                    "additionalRegisters": {},
                    "transactionId": VALID_TX_ID,
                    "index": 0
                }],
                "total": 42
            })))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_str("", &mock_server.uri())
            .await
            .unwrap();
        let paged = node
            .boxes_by_address(&"address".to_string(), 0, 10)
            .await
            .unwrap();
        assert_eq!(paged.total, 42);
        assert_eq!(paged.items.len(), 1);
    }

    #[tokio::test]
    async fn test_paged_boxes_by_token_id() {
        let mock_server = MockServer::start().await;
        let token_id = "0000000000000000000000000000000000000000000000000000000000000000";

        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "indexedHeight": 1000, "fullHeight": 1000
                })),
            )
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!("/blockchain/box/byTokenId/{}", token_id)))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{
                    "boxId": VALID_BOX_ID_WITH_TOKEN,
                    "value": 1000000000u64,
                    "ergoTree": VALID_ERGO_TREE,
                    "creationHeight": 100,
                    "assets": [{ "tokenId": token_id, "amount": 100 }],
                    "additionalRegisters": {},
                    "transactionId": VALID_TX_ID,
                    "index": 0
                }],
                "total": 5
            })))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_str("", &mock_server.uri())
            .await
            .unwrap();
        let digest: Digest32 = Digest32::try_from(token_id.to_string()).unwrap();
        let token: TokenId = TokenId::from(digest);
        let paged = node.boxes_by_token_id(&token, 0, 10).await.unwrap();
        assert_eq!(paged.total, 5);
    }

    #[tokio::test]
    async fn test_unspent_boxes_by_ergo_tree() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "indexedHeight": 1000, "fullHeight": 1000
                })),
            )
            .mount(&mock_server)
            .await;

        // Ergo node unspent endpoints return raw arrays (no items/total wrapper)
        Mock::given(method("POST"))
            .and(path("/blockchain/box/unspent/byErgoTree"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "boxId": VALID_BOX_ID,
                "value": 1000000000u64,
                "ergoTree": VALID_ERGO_TREE,
                "creationHeight": 100,
                "assets": [],
                "additionalRegisters": {},
                "transactionId": VALID_TX_ID,
                "index": 0
            }])))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_str("", &mock_server.uri())
            .await
            .unwrap();
        let boxes = node
            .unspent_boxes_by_ergo_tree(VALID_ERGO_TREE, 0, 10)
            .await
            .unwrap();
        assert_eq!(boxes.len(), 1);
    }

    #[tokio::test]
    async fn test_unspent_boxes_by_ergo_tree_empty_result() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "indexedHeight": 1000, "fullHeight": 1000
                })),
            )
            .mount(&mock_server)
            .await;

        // Empty array response
        Mock::given(method("POST"))
            .and(path("/blockchain/box/unspent/byErgoTree"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_str("", &mock_server.uri())
            .await
            .unwrap();

        let result = node
            .unspent_boxes_by_ergo_tree(VALID_ERGO_TREE, 0, 10)
            .await
            .unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_unspent_boxes_by_ergo_tree_filters_spent_boxes() {
        // Test that the spentTransactionId filter guards against indexer bug
        let mock_server = MockServer::start().await;
        let spent_tx_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "indexedHeight": 1000, "fullHeight": 1000
                })),
            )
            .mount(&mock_server)
            .await;

        // Response includes one unspent box and one spent box (indexer bug)
        // Raw array format (no items/total wrapper)
        Mock::given(method("POST"))
            .and(path("/blockchain/box/unspent/byErgoTree"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "boxId": VALID_BOX_ID,  // Unspent box
                    "value": 1000000000u64,
                    "ergoTree": VALID_ERGO_TREE,
                    "creationHeight": 100,
                    "assets": [],
                    "additionalRegisters": {},
                    "transactionId": VALID_TX_ID,
                    "index": 0,
                    "spentTransactionId": serde_json::Value::Null  // Truly unspent
                },
                {
                    "boxId": VALID_BOX_ID_2,  // Spent box (different content hash)
                    "value": 500000000u64,
                    "ergoTree": VALID_ERGO_TREE,
                    "creationHeight": 50,
                    "assets": [],
                    "additionalRegisters": {},
                    "transactionId": VALID_TX_ID_2,
                    "index": 0,
                    "spentTransactionId": spent_tx_id  // Spent but returned due to indexer bug
                }
            ])))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_str("", &mock_server.uri())
            .await
            .unwrap();
        let boxes = node
            .unspent_boxes_by_ergo_tree(VALID_ERGO_TREE, 0, 10)
            .await
            .unwrap();

        // Only unspent box should be included (spent box filtered out)
        assert_eq!(boxes.len(), 1);
        let box_id_str: String = boxes[0].box_id().into();
        assert_eq!(box_id_str, VALID_BOX_ID);
    }

    #[tokio::test]
    async fn test_paged_transactions_by_address() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "indexedHeight": 1000, "fullHeight": 1000
                })),
            )
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/blockchain/transaction/byAddress"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{ "id": "tx123", "inputs": [], "outputs": [] }],
                "total": 10
            })))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_str("", &mock_server.uri())
            .await
            .unwrap();
        let paged = node
            .transactions_by_address(&"address".to_string(), 0, 10)
            .await
            .unwrap();
        assert_eq!(paged.total, 10);
        assert_eq!(paged.items.len(), 1);
    }

    #[tokio::test]
    async fn test_serialized_box_with_pool_consistency() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path(format!("/utxo/withPool/byIdBinary/{}", VALID_BOX_ID)))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bytes": "80a8d6b907100204a00b08cd0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798ea02d192a39a8cc7a70173007301"
            })))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_without_probe(
            "",
            reqwest::Url::parse(&mock_server.uri()).unwrap(),
        )
        .unwrap();

        let serialized = node
            .serialized_box_from_id_with_pool(VALID_BOX_ID)
            .await
            .unwrap();
        assert!(serialized.contains("80a8d6b907"));
    }

    #[tokio::test]
    async fn test_strict_paging_errors_on_missing_total() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "indexedHeight": 1000, "fullHeight": 1000
                })),
            )
            .mount(&mock_server)
            .await;

        // Response missing 'total' field
        Mock::given(method("POST"))
            .and(path("/blockchain/box/byAddress"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": []
                // Missing "total"
            })))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_str("", &mock_server.uri())
            .await
            .unwrap();
        let result = node.boxes_by_address(&"address".to_string(), 0, 10).await;
        assert!(matches!(
            result,
            Err(NodeError::FailedParsingNodeResponse(_))
        ));
    }

    #[tokio::test]
    async fn test_box_from_id_with_pool() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path(format!("/utxo/withPool/byId/{}", VALID_BOX_ID)))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "boxId": VALID_BOX_ID,
                "value": 1000000000u64,
                "ergoTree": VALID_ERGO_TREE,
                "creationHeight": 100,
                "assets": [],
                "additionalRegisters": {},
                "transactionId": VALID_TX_ID,
                "index": 0
            })))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_without_probe(
            "",
            reqwest::Url::parse(&mock_server.uri()).unwrap(),
        )
        .unwrap();

        let ergo_box = node.box_from_id_with_pool(VALID_BOX_ID).await.unwrap();
        let box_id_str: String = ergo_box.box_id().into();
        assert_eq!(box_id_str, VALID_BOX_ID);
        assert_eq!(*ergo_box.value.as_u64(), 1000000000u64);
    }

    #[tokio::test]
    async fn test_nano_ergs_balance() {
        let mock_server = MockServer::start().await;

        // Probe returns 200 -> extraIndex enabled
        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "indexedHeight": 1000, "fullHeight": 1000
                })),
            )
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/blockchain/balance"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "confirmed": {
                    "nanoErgs": 5000000000u64,
                    "tokens": []
                },
                "unconfirmed": {
                    "nanoErgs": 0,
                    "tokens": []
                }
            })))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_str("", &mock_server.uri())
            .await
            .unwrap();

        let balance = node
            .nano_ergs_balance(&"9fRAWhdxEsTcdb8PhGNrZfwqa65zfkuYHAMmkQLcic1gdLSV5vA".to_string())
            .await
            .unwrap();
        assert_eq!(balance, 5000000000u64);
    }

    #[tokio::test]
    async fn test_nano_ergs_balance_requires_extra_index() {
        let mock_server = MockServer::start().await;

        // Probe returns 404 -> extraIndex disabled
        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_str("", &mock_server.uri())
            .await
            .unwrap();
        assert_eq!(node.has_extra_index(), Some(false));

        let result = node
            .nano_ergs_balance(&"9fRAWhdxEsTcdb8PhGNrZfwqa65zfkuYHAMmkQLcic1gdLSV5vA".to_string())
            .await;
        assert!(matches!(result, Err(NodeError::ExtraIndexRequired)));
    }

    #[tokio::test]
    async fn test_get_token_info() {
        let mock_server = MockServer::start().await;
        let token_id = "0000000000000000000000000000000000000000000000000000000000000000";

        // Probe returns 200 -> extraIndex enabled
        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "indexedHeight": 1000, "fullHeight": 1000
                })),
            )
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!("/blockchain/token/byId/{}", token_id)))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": token_id,
                "boxId": VALID_BOX_ID_WITH_TOKEN,
                "emissionAmount": 1000000,
                "name": "Test Token",
                "description": "A test token",
                "decimals": 0
            })))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_str("", &mock_server.uri())
            .await
            .unwrap();

        let token_info = node.get_token_info(token_id).await.unwrap();
        assert_eq!(token_info["id"], token_id);
        assert_eq!(token_info["name"], "Test Token");
        assert_eq!(token_info["emissionAmount"], 1000000);
    }

    #[tokio::test]
    async fn test_get_token_info_requires_extra_index() {
        let mock_server = MockServer::start().await;
        let token_id = "0000000000000000000000000000000000000000000000000000000000000000";

        // Probe returns 404 -> extraIndex disabled
        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let node = NodeInterface::from_url_str("", &mock_server.uri())
            .await
            .unwrap();
        assert_eq!(node.has_extra_index(), Some(false));

        let result = node.get_token_info(token_id).await;
        assert!(matches!(result, Err(NodeError::ExtraIndexRequired)));
    }
}
