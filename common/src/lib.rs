use serde::{Deserialize, Serialize};

/// Messages sent over the WebSocket control channel
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    /// Client → Server: register and request a tunnel
    Register {
        /// Optional desired subdomain; server assigns one if None
        subdomain: Option<String>,
        /// Secret token for auth (set via env SERVER_SECRET)
        token: String,
    },

    /// Server → Client: tunnel ready
    Registered {
        /// Assigned subdomain, e.g. "abc123"
        subdomain: String,
        /// Full public URL
        public_url: String,
    },

    /// Server → Client: a new HTTP request arrived
    RequestIncoming {
        /// Unique ID for this request/response pair
        request_id: String,
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        /// Base64-encoded body (may be empty)
        body_b64: String,
    },

    /// Client → Server: HTTP response from local service
    ResponseOutgoing {
        request_id: String,
        status: u16,
        headers: Vec<(String, String)>,
        /// Base64-encoded body
        body_b64: String,
    },

    /// Either direction: keep-alive ping
    Ping,
    /// Either direction: keep-alive pong
    Pong,

    /// Server → Client: auth failed or other error
    Error { message: String },
}

impl ControlMessage {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("serialize ControlMessage")
    }

    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}
