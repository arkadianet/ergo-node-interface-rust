/// Functions related to saving/accessing local data
/// for interacting with an Ergo Node. (Ip/Port/Api Key)
use crate::node_interface::{NodeError, NodeInterface, Result};
use std::fs::File;
use std::io::prelude::*;
use std::path::Path;
use yaml_rust::{Yaml, YamlLoader};

static BAREBONES_CONFIG_YAML: &str = r#"
# IP Address of the node (default is local, edit if yours is different)
node_ip: "0.0.0.0"
# Port that the node is on (default is 9053, edit if yours is different)
node_port: "9053"
# API key for the node (edit if yours is different)
node_api_key: "hello"
"#;

/// Default config path
const DEFAULT_CONFIG_PATH: &str = "node-interface.yaml";

/// A ease-of-use function which attempts to acquire a `NodeInterface`
/// from a local file. If the file does not exist, it generates a new
/// config file, tells the user to edit the config file, and then closes
/// the running application
/// This is useful for CLI applications, however should not be used by
/// GUI-based applications.
pub fn acquire_node_interface_from_local_config() -> NodeInterface {
    // `Node-interface.yaml` setup logic
    if !does_local_config_exist() {
        println!("Could not find local `node-interface.yaml` file.\nCreating said file with basic defaults.\nPlease edit the yaml file and update it with your node parameters to ensure the CLI app can proceed.");
        create_new_local_config_file().ok();
        std::process::exit(0);
    }
    // Error checking reading the local node interface yaml
    if let Err(e) = new_interface_from_local_config() {
        println!("Could not parse local `node-interface.yaml` file.\nError: {e:?}");
        std::process::exit(0);
    }
    // Create `NodeInterface`
    new_interface_from_local_config().unwrap()
}

/// Basic function to check if a local config currently exists
pub fn does_local_config_exist() -> bool {
    Path::new(DEFAULT_CONFIG_PATH).exists()
}

/// Create a new `node-interface.config` with the barebones yaml inside
pub fn create_new_local_config_file() -> Result<()> {
    let file_path = Path::new(DEFAULT_CONFIG_PATH);
    if !file_path.exists() {
        let mut file = File::create(file_path).map_err(|_| {
            NodeError::YamlError("Failed to create `node-interface.yaml` file".to_string())
        })?;
        file.write_all(&BAREBONES_CONFIG_YAML.to_string().into_bytes())
            .map_err(|_| {
                NodeError::YamlError(
                    "Failed to write to local `node-interface.yaml` file".to_string(),
                )
            })?;
        return Ok(());
    }
    Err(NodeError::YamlError(
        "Local `node-interface.yaml` already exists.".to_string(),
    ))
}

// ===== Helper functions =====

fn load_yaml_from_file(path: &str) -> Result<Yaml> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| NodeError::YamlError(format!("Failed to read config file '{}': {}", path, e)))?;
    parse_yaml(&contents)
}

fn parse_yaml(yaml_str: &str) -> Result<Yaml> {
    let docs = YamlLoader::load_from_str(yaml_str)
        .map_err(|e| NodeError::YamlError(format!("Failed to parse YAML: {}", e)))?;
    if docs.is_empty() {
        return Err(NodeError::YamlError("Empty YAML document".to_string()));
    }
    Ok(docs[0].clone())
}

fn parse_api_key(config: &Yaml) -> Result<String> {
    config["node_api_key"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| {
            NodeError::YamlError("`node_api_key` is not specified in the provided Yaml".to_string())
        })
}

fn parse_ip(config: &Yaml) -> Result<String> {
    config["node_ip"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| {
            NodeError::YamlError("`node_ip` is not specified in the provided Yaml".to_string())
        })
}

fn parse_port(config: &Yaml) -> Result<String> {
    config["node_port"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| {
            NodeError::YamlError("`node_port` is not specified in the provided Yaml".to_string())
        })
}

// ===== YAML-based constructors =====

/// Uses the config yaml to create a NodeInterface without probing.
///
/// Capability is unknown (None). Call `refresh_capabilities()` to probe,
/// or use `new_interface_from_yaml_async()` for auto-detection.
pub fn new_interface_from_yaml(config: Yaml) -> Result<NodeInterface> {
    let api_key = parse_api_key(&config)?;
    let ip = parse_ip(&config)?;
    let port = parse_port(&config)?;
    NodeInterface::new_without_probe(&api_key, &ip, &port)
}

/// Async version that probes for extraIndex capability.
pub async fn new_interface_from_yaml_async(config: Yaml) -> Result<NodeInterface> {
    let api_key = parse_api_key(&config)?;
    let ip = parse_ip(&config)?;
    let port = parse_port(&config)?;
    NodeInterface::new(&api_key, &ip, &port).await
}

// ===== File path-based constructors =====

/// Load config from specified file path and create NodeInterface without probing.
/// Use this when you have a custom config location.
pub fn new_interface_with_path(config_path: &str) -> Result<NodeInterface> {
    let config = load_yaml_from_file(config_path)?;
    new_interface_from_yaml(config)
}

/// Async: Load config from specified file path and create NodeInterface with probing.
pub async fn new_interface_with_path_async(config_path: &str) -> Result<NodeInterface> {
    let config = load_yaml_from_file(config_path)?;
    new_interface_from_yaml_async(config).await
}

// ===== Default location constructors =====

/// Load config from default location (`node-interface.yaml`) and create
/// NodeInterface without probing.
///
/// Existing function - signature preserved for backward compatibility.
pub fn new_interface_from_local_config() -> Result<NodeInterface> {
    let config = load_yaml_from_file(DEFAULT_CONFIG_PATH)?;
    new_interface_from_yaml(config)
}

/// Async: Load config from default location (`node-interface.yaml`) and create
/// NodeInterface with probing for extraIndex capability.
pub async fn new_interface_from_local_config_async() -> Result<NodeInterface> {
    let config = load_yaml_from_file(DEFAULT_CONFIG_PATH)?;
    new_interface_from_yaml_async(config).await
}
