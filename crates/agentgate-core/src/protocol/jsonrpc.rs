use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 2.0 request.
///
/// `params` is stored as a `serde_json::Value` so that the `#[serde(untagged)]`
/// discriminator on `JsonRpcMessage` continues to work. (`Box<RawValue>` cannot
/// be deserialised through serde_json's internal `Value` buffer that untagged
/// enums rely on.)
///
/// For zero-copy argument extraction on the hot path use `extract_tool_params`,
/// which parses tool name and arguments from the *raw line* before the full
/// `JsonRpcMessage` allocation — bypassing the untagged-enum buffer entirely.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    /// id is nullable; notifications have no id field (represented as None here via
    /// `skip_serializing_if`). We use `default` so missing id deserialises as None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 error object
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// JSON-RPC 2.0 response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// Either a request or a response — used when deserialising an unknown incoming message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcMessage {
    Request(JsonRpcRequest),
    Response(JsonRpcResponse),
}

impl JsonRpcMessage {
    /// Parse a line of text into a `JsonRpcMessage`.
    pub fn parse(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }

    /// Return the method name if this is a request.
    pub fn method(&self) -> Option<&str> {
        match self {
            JsonRpcMessage::Request(r) => Some(&r.method),
            JsonRpcMessage::Response(_) => None,
        }
    }

    /// Return the id value (as a string label) for logging.
    pub fn id_label(&self) -> String {
        match self {
            JsonRpcMessage::Request(r) => {
                r.id.as_ref()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "notification".to_string())
            }
            JsonRpcMessage::Response(r) => r.id.to_string(),
        }
    }
}

/// Extract `(tool_name, arguments)` from a `tools/call` request.
/// Centralised here so all three proxy transports share the same extraction logic
/// rather than duplicating the `.get("name")` / `.get("arguments")` pattern.
pub fn extract_tool_params(req: &JsonRpcRequest) -> (String, Option<Value>) {
    let Some(params) = &req.params else {
        return ("unknown".to_string(), None);
    };
    let tool_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let arguments = params.get("arguments").cloned();
    (tool_name, arguments)
}

/// Rebuild a `tools/call` request with a replacement `arguments` value.
/// Centralised here so the stdio proxy can call it after policy redaction.
pub fn rebuild_tool_call(
    original: &JsonRpcRequest,
    new_arguments: Option<Value>,
) -> JsonRpcRequest {
    let mut params = original
        .params
        .clone()
        .unwrap_or(Value::Object(Default::default()));
    if let Value::Object(ref mut map) = params {
        match new_arguments {
            Some(args) => {
                map.insert("arguments".to_string(), args);
            }
            None => {
                map.remove("arguments");
            }
        }
    }
    JsonRpcRequest {
        jsonrpc: original.jsonrpc.clone(),
        id: original.id.clone(),
        method: original.method.clone(),
        params: Some(params),
    }
}
