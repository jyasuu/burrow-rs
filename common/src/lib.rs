//! Wire protocol for the burrow-rs HTTP reverse tunnel.
//!
//! All communication between the server and client happens over a WebSocket
//! connection using JSON-serialized [`ControlMessage`] values. The protocol
//! uses a tagged enum (`{ "type": "variant_name", ... }`) for framing.
//!
//! # Protocol flow
//!
//! 1. Client sends [`Register`](ControlMessage::Register) with auth token.
//! 2. Server responds with [`Registered`](ControlMessage::Registered) on success,
//!    or [`Error`](ControlMessage::Error) on failure.
//! 3. For each incoming HTTP request, server sends
//!    [`RequestIncoming`](ControlMessage::RequestIncoming).
//! 4. Client forwards to localhost, then replies with
//!    [`ResponseOutgoing`](ControlMessage::ResponseOutgoing).
//! 5. Server sends [`Ping`](ControlMessage::Ping) every 30s for keep-alive;
//!    client responds with [`Pong`](ControlMessage::Pong).
//!
//! # Example
//!
//! ```ignore
//! use burrow_common::ControlMessage;
//!
//! let register = ControlMessage::Register {
//!     subdomain: Some("myapp".into()),
//!     token: "secret".into(),
//! };
//! let json = register.to_json();
//! let decoded = ControlMessage::from_json(&json).unwrap();
//! ```

use serde::{Deserialize, Serialize};

/// A message sent over the burrow WebSocket control channel.
///
/// Each variant is serialized as a JSON object with a `"type"` field
/// (e.g. `{"type": "ping"}`). The serialization format is stable across
/// server and client versions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    /// Client → Server: register and request a tunnel.
    Register {
        /// Desired subdomain. The server assigns a random one if `None`.
        subdomain: Option<String>,
        /// Auth token. Must match the server's `SERVER_SECRET`.
        token: String,
    },

    /// Server → Client: tunnel is ready.
    Registered {
        /// The assigned subdomain (e.g. `"abc123"` or the requested one).
        subdomain: String,
        /// Full public URL (e.g. `"https://burrow.example.com/myapp"`).
        public_url: String,
    },

    /// Server → Client: an incoming HTTP request to forward.
    RequestIncoming {
        /// Unique ID correlating request and response.
        request_id: String,
        /// HTTP method (e.g. `"GET"`, `"POST"`).
        method: String,
        /// Request path (e.g. `"/api/foo?q=1"`).
        path: String,
        /// Request headers as key-value pairs.
        headers: Vec<(String, String)>,
        /// Base64-encoded request body (empty string if no body).
        body_b64: String,
    },

    /// Client → Server: the HTTP response from the local service.
    ResponseOutgoing {
        /// Must match the `request_id` from the corresponding `RequestIncoming`.
        request_id: String,
        /// HTTP status code (e.g. `200`, `404`).
        status: u16,
        /// Response headers as key-value pairs.
        headers: Vec<(String, String)>,
        /// Base64-encoded response body.
        body_b64: String,
    },

    /// Either direction: keep-alive ping (server sends every 30s).
    Ping,
    /// Either direction: keep-alive pong (client auto-replies).
    Pong,

    /// Server → Client: an error occurred (auth failure, bad request, etc.).
    Error {
        /// Human-readable error description.
        message: String,
    },
}

impl ControlMessage {
    /// Serialize this message to a JSON string.
    ///
    /// # Panics
    ///
    /// Panics if serialization fails (should never happen for this type).
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("serialize ControlMessage")
    }

    /// Deserialize a message from a JSON string.
    ///
    /// Returns `Err` if the JSON is malformed or the `"type"` tag is unknown.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_register() {
        let msg = ControlMessage::Register {
            subdomain: Some("myapp".into()),
            token: "secret".into(),
        };
        let json = msg.to_json();
        let decoded = ControlMessage::from_json(&json).unwrap();
        match decoded {
            ControlMessage::Register { subdomain, token } => {
                assert_eq!(subdomain, Some("myapp".into()));
                assert_eq!(token, "secret");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn round_trip_registered() {
        let msg = ControlMessage::Registered {
            subdomain: "abc123".into(),
            public_url: "https://example.com/abc123".into(),
        };
        let json = msg.to_json();
        let decoded = ControlMessage::from_json(&json).unwrap();
        match decoded {
            ControlMessage::Registered { subdomain, public_url } => {
                assert_eq!(subdomain, "abc123");
                assert_eq!(public_url, "https://example.com/abc123");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn round_trip_request_incoming() {
        let msg = ControlMessage::RequestIncoming {
            request_id: "req-1".into(),
            method: "POST".into(),
            path: "/api/data".into(),
            headers: vec![("content-type".into(), "application/json".into())],
            body_b64: "aGVsbG8=".into(),
        };
        let json = msg.to_json();
        let decoded = ControlMessage::from_json(&json).unwrap();
        match decoded {
            ControlMessage::RequestIncoming { request_id, method, path, headers, body_b64 } => {
                assert_eq!(request_id, "req-1");
                assert_eq!(method, "POST");
                assert_eq!(path, "/api/data");
                assert_eq!(headers[0], ("content-type".into(), "application/json".into()));
                assert_eq!(body_b64, "aGVsbG8=");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn round_trip_response_outgoing() {
        let msg = ControlMessage::ResponseOutgoing {
            request_id: "req-1".into(),
            status: 200,
            headers: vec![("x-custom".into(), "val".into())],
            body_b64: "".into(),
        };
        let json = msg.to_json();
        let decoded = ControlMessage::from_json(&json).unwrap();
        match decoded {
            ControlMessage::ResponseOutgoing { request_id, status, headers, body_b64 } => {
                assert_eq!(request_id, "req-1");
                assert_eq!(status, 200);
                assert_eq!(headers[0], ("x-custom".into(), "val".into()));
                assert_eq!(body_b64, "");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn round_trip_ping_pong() {
        let ping = ControlMessage::Ping;
        let pong = ControlMessage::Pong;
        let j1 = ping.to_json();
        let j2 = pong.to_json();
        assert_eq!(j1, r#"{"type":"ping"}"#);
        assert_eq!(j2, r#"{"type":"pong"}"#);
        match ControlMessage::from_json(&j1).unwrap() {
            ControlMessage::Ping => {}
            _ => panic!("expected Ping"),
        }
        match ControlMessage::from_json(&j2).unwrap() {
            ControlMessage::Pong => {}
            _ => panic!("expected Pong"),
        }
    }

    #[test]
    fn round_trip_error() {
        let msg = ControlMessage::Error { message: "something went wrong".into() };
        let json = msg.to_json();
        let decoded = ControlMessage::from_json(&json).unwrap();
        match decoded {
            ControlMessage::Error { message } => {
                assert_eq!(message, "something went wrong");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn json_tag_snake_case() {
        assert_eq!(
            ControlMessage::Ping.to_json(),
            r#"{"type":"ping"}"#
        );
        assert_eq!(
            ControlMessage::Pong.to_json(),
            r#"{"type":"pong"}"#
        );
        let reg = ControlMessage::Register {
            subdomain: None,
            token: "t".into(),
        };
        assert!(reg.to_json().contains(r#""type":"register""#));
        let req = ControlMessage::RequestIncoming {
            request_id: "r".into(),
            method: "GET".into(),
            path: "/".into(),
            headers: vec![],
            body_b64: "".into(),
        };
        assert!(req.to_json().contains(r#""type":"request_incoming""#));
        let resp = ControlMessage::ResponseOutgoing {
            request_id: "r".into(),
            status: 200,
            headers: vec![],
            body_b64: "".into(),
        };
        assert!(resp.to_json().contains(r#""type":"response_outgoing""#));
    }

    #[test]
    fn invalid_json_returns_err() {
        let result = ControlMessage::from_json("not valid json");
        assert!(result.is_err());
    }

    #[test]
    fn unknown_type_returns_err() {
        let result = ControlMessage::from_json(r#"{"type":"unknown"}"#);
        assert!(result.is_err());
    }
}
