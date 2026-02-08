use std::convert::TryFrom;

use crate::node_interface::{NodeError, NodeInterface, Result};
use crate::{JsonString, JsonValue};
use ergo_lib::chain::transaction::unsigned::UnsignedTransaction;
use ergo_lib::chain::transaction::{Transaction, TxId};
use ergo_lib::ergo_chain_types::Digest32;
use ergo_lib::ergotree_ir::chain::ergo_box::ErgoBox;
use ergo_lib::ergotree_ir::serialization::{SigmaSerializable, SigmaSerializationError};
use ergo_lib::wallet::signing::TransactionContext;
use serde_json::json;

impl NodeInterface {
    /// Submits a Signed Transaction provided as input as JSON
    /// to the Ergo Blockchain mempool.
    pub async fn submit_json_transaction(&self, signed_tx_json: &JsonString) -> Result<TxId> {
        let endpoint = "/transactions";
        let res_json = self
            .use_json_endpoint_and_check_errors(endpoint, signed_tx_json)
            .await?;
        let tx_id = parse_tx_id_unsafe(res_json);
        Ok(tx_id)
    }

    /// Sign an Unsigned Transaction which is formatted in JSON
    pub async fn sign_json_transaction(
        &self,
        unsigned_tx_string: &JsonString,
    ) -> Result<JsonValue> {
        let endpoint = "/wallet/transaction/sign";
        let unsigned_tx_json: JsonValue = serde_json::from_str(unsigned_tx_string)
            .map_err(|_| NodeError::FailedParsingNodeResponse(unsigned_tx_string.to_string()))?;

        let prepared_body = json!({
            "tx": unsigned_tx_json
        });

        let res_json = self
            .use_json_endpoint_and_check_errors(endpoint, &prepared_body.to_string())
            .await?;

        Ok(res_json)
    }

    /// Sign an Unsigned Transaction which is formatted in JSON
    /// and then submit it to the mempool.
    pub async fn sign_and_submit_json_transaction(
        &self,
        unsigned_tx_string: &JsonString,
    ) -> Result<TxId> {
        let signed_tx = self.sign_json_transaction(unsigned_tx_string).await?;
        let signed_tx_json = serde_json::to_string(&signed_tx)
            .map_err(|_| NodeError::Other("Failed Converting `JsonValue` to string".to_string()))?;

        self.submit_json_transaction(&signed_tx_json).await
    }

    /// Submits a Signed `Transaction` provided as input
    /// to the Ergo Blockchain mempool.
    pub async fn submit_transaction(&self, signed_tx: &Transaction) -> Result<TxId> {
        let signed_tx_json = &serde_json::to_string(&signed_tx)
            .map_err(|_| NodeError::Other("Failed Converting `Transaction` to json".to_string()))?;
        let tx_id = self.submit_json_transaction(signed_tx_json).await?;
        if tx_id != signed_tx.id() {
            return Err(NodeError::Other(format!(
                "Transaction ID mismatch: expected {}, got {}",
                signed_tx.id(),
                tx_id
            )));
        }
        Ok(tx_id)
    }

    /// Sign an `UnsignedTransaction`
    /// unsigned_tx - The unsigned transaction to sign.
    /// boxes_to_spend - optional list of input boxes. If not provided, the node will search for the boxes in UTXO
    /// data_input_boxes - optional list of data boxes. If not provided, the node will search for the data boxes in UTXO
    pub async fn sign_transaction(
        &self,
        unsigned_tx: &UnsignedTransaction,
        boxes_to_spend: Option<Vec<ErgoBox>>,
        data_input_boxes: Option<Vec<ErgoBox>>,
    ) -> Result<Transaction> {
        if let Some(ref boxes_to_spend) = boxes_to_spend {
            // check input boxes against tx's inputs (for every input should be a box)
            if let Err(e) = TransactionContext::new(
                unsigned_tx.clone(),
                boxes_to_spend.clone(),
                data_input_boxes.clone().unwrap_or_default(),
            ) {
                return Err(NodeError::Other(e.to_string()));
            };
        }

        let endpoint = "/wallet/transaction/sign";

        fn encode_boxes(
            maybe_boxes: Option<Vec<ErgoBox>>,
        ) -> std::result::Result<Option<Vec<String>>, NodeError> {
            match maybe_boxes.map(|boxes| {
                boxes
                    .iter()
                    .map(|b| {
                        b.sigma_serialize_bytes()
                            .map(|bytes| base16::encode_lower(&bytes))
                    })
                    .collect::<std::result::Result<Vec<String>, SigmaSerializationError>>()
            }) {
                Some(Ok(base16_boxes)) => Ok(Some(base16_boxes)),
                Some(Err(e)) => Err(NodeError::Other(e.to_string())),
                None => Ok(None),
            }
        }

        let input_boxes_base16 = encode_boxes(boxes_to_spend)?;
        let data_input_boxes_base16 = encode_boxes(data_input_boxes)?;

        let prepared_body = json!({
            "tx": unsigned_tx,
            "inputsRaw": input_boxes_base16,
            "dataInputsRaw": data_input_boxes_base16,
        });

        let json_signed_tx = self
            .use_json_endpoint_and_check_errors(endpoint, &prepared_body.to_string())
            .await?;

        serde_json::from_value(json_signed_tx)
            .map_err(|_| NodeError::Other("Failed Converting `Transaction` from json".to_string()))
    }

    /// Sign an `UnsignedTransaction` and then submit it to the mempool.
    pub async fn sign_and_submit_transaction(
        &self,
        unsigned_tx: &UnsignedTransaction,
    ) -> Result<TxId> {
        let signed_tx = self.sign_transaction(unsigned_tx, None, None).await?;
        self.submit_transaction(&signed_tx).await
    }

    /// Generates and submits a tx using the node endpoints. Input is
    /// a json formatted request with rawInputs (and rawDataInputs)
    /// manually selected or inputs will be automatically selected by wallet.
    /// Returns the resulting `TxId`.
    pub async fn generate_and_submit_transaction(
        &self,
        tx_request_json: &JsonString,
    ) -> Result<TxId> {
        let endpoint = "/wallet/transaction/send";
        let res_json = self
            .use_json_endpoint_and_check_errors(endpoint, tx_request_json)
            .await?;
        let tx_id = parse_tx_id_unsafe(res_json);
        Ok(tx_id)
    }

    /// Generates Json of an Unsigned Transaction.
    /// Input must be a json formatted request with rawInputs (and rawDataInputs)
    /// manually selected or will be automatically selected by wallet.
    pub async fn generate_json_transaction(
        &self,
        tx_request_json: &JsonString,
    ) -> Result<JsonValue> {
        let endpoint = "/wallet/transaction/generate";
        let res_json = self
            .use_json_endpoint_and_check_errors(endpoint, tx_request_json)
            .await?;

        Ok(res_json)
    }

    /// Gets the recommended fee for a transaction.
    /// bytes - size of the transaction in bytes
    /// wait_time - minutes to wait for the transaction to be included in the blockchain
    pub async fn get_recommended_fee(&self, bytes: u64, wait_time: u64) -> Result<u64> {
        let endpoint = format!(
            "/transactions/getFee?bytes={}&waitTime={}",
            bytes, wait_time
        );
        let res = self.send_get_req(&endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;
        res_json
            .as_u64()
            .ok_or_else(|| NodeError::FailedParsingNodeResponse(res_json.to_string()))
    }

    /// Checks a Signed Transaction provided as input as JSON
    /// without submitting it to the mempool. Returns transaction ID if valid.
    pub async fn check_json_transaction(&self, signed_tx_json: &JsonString) -> Result<TxId> {
        let endpoint = "/transactions/check";
        let res_json = self
            .use_json_endpoint_and_check_errors(endpoint, signed_tx_json)
            .await?;
        let tx_id = parse_tx_id_unsafe(res_json);
        Ok(tx_id)
    }

    /// Checks a Signed `Transaction` provided as input
    /// without submitting it to the mempool. Returns transaction ID if valid.
    pub async fn check_transaction(&self, signed_tx: &Transaction) -> Result<TxId> {
        let signed_tx_json = &serde_json::to_string(&signed_tx)
            .map_err(|_| NodeError::Other("Failed Converting `Transaction` to json".to_string()))?;
        self.check_json_transaction(signed_tx_json).await
    }

    /// Checks a transaction provided as hex-encoded bytes
    /// without submitting it to the mempool. Returns transaction ID if valid.
    pub async fn check_transaction_bytes(&self, tx_bytes_hex: &str) -> Result<TxId> {
        let endpoint = "/transactions/checkBytes";
        let res_json = self
            .use_json_endpoint_and_check_errors(endpoint, &tx_bytes_hex.to_string())
            .await?;
        let tx_id = parse_tx_id_unsafe(res_json);
        Ok(tx_id)
    }

    /// Submits a transaction provided as hex-encoded bytes
    /// to the Ergo Blockchain mempool.
    pub async fn submit_transaction_bytes(&self, tx_bytes_hex: &str) -> Result<TxId> {
        let endpoint = "/transactions/bytes";
        let res_json = self
            .use_json_endpoint_and_check_errors(endpoint, &tx_bytes_hex.to_string())
            .await?;
        let tx_id = parse_tx_id_unsafe(res_json);
        Ok(tx_id)
    }

    /// Gets unconfirmed transactions from the mempool.
    pub async fn mempool_transactions(&self) -> Result<Vec<JsonValue>> {
        let endpoint = "/transactions/unconfirmed";
        let res = self.send_get_req(endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;

        let mut transactions = vec![];
        for i in 0.. {
            let tx_json = &res_json[i];
            if tx_json.is_null() {
                break;
            } else {
                transactions.push(tx_json.clone());
            }
        }
        Ok(transactions)
    }

    /// Gets a specific unconfirmed transaction from the mempool by transaction ID.
    pub async fn unconfirmed_transaction_by_id(&self, tx_id: &str) -> Result<JsonValue> {
        let endpoint = format!("/transactions/unconfirmed/byTransactionId/{}", tx_id);
        let res = self.send_get_req(&endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;

        Ok(res_json)
    }

    /// Finds unconfirmed transactions by ErgoTree hex of one of its output or
    /// input boxes (if present in UtxoState).
    ///
    /// This is the key method for getting mempool transactions related to a
    /// specific address. Convert an address to its ErgoTree hex first, then
    /// pass it here.
    pub async fn unconfirmed_transactions_by_ergo_tree(
        &self,
        ergo_tree_hex: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<JsonValue>> {
        let endpoint = format!(
            "/transactions/unconfirmed/byErgoTree?offset={}&limit={}",
            offset, limit
        );
        let res = self.send_post_req(&endpoint, ergo_tree_hex.to_string()).await;
        let res_json = self.parse_response_to_json(res).await?;

        let mut transactions = vec![];
        for i in 0.. {
            let tx_json = &res_json[i];
            if tx_json.is_null() {
                break;
            } else {
                transactions.push(tx_json.clone());
            }
        }
        Ok(transactions)
    }

    /// Get an input box from unconfirmed transactions in the mempool by box ID.
    ///
    /// Returns the box that is being spent by an unconfirmed transaction.
    pub async fn unconfirmed_input_by_box_id(&self, box_id: &str) -> Result<JsonValue> {
        let endpoint = format!("/transactions/unconfirmed/inputs/byBoxId/{}", box_id);
        let res = self.send_get_req(&endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;

        Ok(res_json)
    }

    /// Get an output box from unconfirmed transactions in the mempool by box ID.
    ///
    /// Returns the box that is being created by an unconfirmed transaction.
    pub async fn unconfirmed_output_by_box_id(&self, box_id: &str) -> Result<JsonValue> {
        let endpoint = format!("/transactions/unconfirmed/outputs/byBoxId/{}", box_id);
        let res = self.send_get_req(&endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;

        Ok(res_json)
    }

    /// Finds all output boxes by ErgoTree hex among unconfirmed transactions.
    ///
    /// Useful for finding pending outputs destined to a specific address
    /// (by its ErgoTree representation).
    pub async fn unconfirmed_outputs_by_ergo_tree(
        &self,
        ergo_tree_hex: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<JsonValue>> {
        let endpoint = format!(
            "/transactions/unconfirmed/outputs/byErgoTree?offset={}&limit={}",
            offset, limit
        );
        let res = self.send_post_req(&endpoint, ergo_tree_hex.to_string()).await;
        let res_json = self.parse_response_to_json(res).await?;

        let mut outputs = vec![];
        for i in 0.. {
            let box_json = &res_json[i];
            if box_json.is_null() {
                break;
            } else {
                outputs.push(box_json.clone());
            }
        }
        Ok(outputs)
    }

    /// Get output boxes from unconfirmed transactions that contain a given token.
    pub async fn unconfirmed_outputs_by_token_id(
        &self,
        token_id: &str,
    ) -> Result<Vec<JsonValue>> {
        let endpoint = format!("/transactions/unconfirmed/outputs/byTokenId/{}", token_id);
        let res = self.send_get_req(&endpoint).await;
        let res_json = self.parse_response_to_json(res).await?;

        let mut outputs = vec![];
        for i in 0.. {
            let box_json = &res_json[i];
            if box_json.is_null() {
                break;
            } else {
                outputs.push(box_json.clone());
            }
        }
        Ok(outputs)
    }
}

fn parse_tx_id_unsafe(res_json: JsonValue) -> TxId {
    // If tx is valid and is posted, return just the tx id
    let tx_id_str = match res_json.as_str() {
        Some(s) => s.to_string(),
        None => {
            // Fallback: if it's not a string, convert the whole value to string and remove quotes
            let full_string = res_json.to_string();
            full_string.trim_matches('"').to_string()
        }
    };
    TxId(Digest32::try_from(tx_id_str).unwrap())
}
