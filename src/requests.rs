use crate::node_interface::{NodeError, NodeInterface, Result};
use crate::{JsonString, JsonValue};
use reqwest::header::{HeaderValue, CONTENT_TYPE};
use reqwest::{RequestBuilder, Response};

impl NodeInterface {
    /// Builds a `HeaderValue` to use for requests with the api key specified
    pub fn get_node_api_header(&self) -> HeaderValue {
        match HeaderValue::from_str(&self.api_key) {
            Ok(k) => k,
            _ => HeaderValue::from_static("None"),
        }
    }

    /// Sets required headers for a request
    pub fn set_req_headers(&self, rb: RequestBuilder) -> RequestBuilder {
        rb.header("accept", "application/json")
            .header("api_key", self.get_node_api_header())
            .header(CONTENT_TYPE, "application/json")
    }

    /// Sends a GET request to the Ergo node
    pub async fn send_get_req(&self, endpoint: &str) -> Result<Response> {
        let url = self
            .url
            .join(endpoint)
            .map_err(|e| NodeError::InvalidUrl(e.to_string()))?;
        self.set_req_headers(self.client.get(url))
            .send()
            .await
            .map_err(|_| NodeError::NodeUnreachable)
    }

    /// Sends a POST request to the Ergo node
    pub async fn send_post_req(&self, endpoint: &str, body: String) -> Result<Response> {
        let url = self
            .url
            .join(endpoint)
            .map_err(|e| NodeError::InvalidUrl(e.to_string()))?;
        self.set_req_headers(self.client.post(url))
            .body(body)
            .send()
            .await
            .map_err(|_| NodeError::NodeUnreachable)
    }

    /// Parses response from node into JSON
    pub async fn parse_response_to_json(&self, resp: Result<Response>) -> Result<JsonValue> {
        let text = resp?.text().await.map_err(|_| {
            NodeError::FailedParsingNodeResponse(
                "Node Response Not Parseable into Text.".to_string(),
            )
        })?;
        let json =
            serde_json::from_str(&text).map_err(|_| NodeError::FailedParsingNodeResponse(text))?;
        Ok(json)
    }

    /// General function for submitting a Json String body to an endpoint
    /// which also returns a `JsonValue` response.
    pub async fn use_json_endpoint_and_check_errors(
        &self,
        endpoint: &str,
        json_body: &JsonString,
    ) -> Result<JsonValue> {
        let res = self.send_post_req(endpoint, json_body.to_string()).await;

        let res_json = self.parse_response_to_json(res).await?;
        let error_details = res_json["detail"].to_string();

        // Check if send tx request failed and returned error json
        if error_details != "null" {
            return Err(NodeError::BadRequest(error_details));
        }

        Ok(res_json)
    }
}
