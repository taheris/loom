//! Shared canonicalization + BLAKE3-16 hashing pipeline both observers
//! consume. Per `specs/llm.md` the utility lives in exactly one place;
//! [`crate::Conversation`] fingerprints each live tool result once and
//! fans that value into [`super::doom_loop`] and
//! [`super::duplicate_result`].

use displaydoc::Display;
use serde_json::Value;

/// Errors from RFC 8785 JSON canonicalization.
#[derive(Debug, Display, thiserror::Error)]
pub enum Error {
    /// RFC 8785 JSON canonicalization failed
    Canonicalization(#[from] serde_json::Error),
}

/// Per-call identity formed from a tool name and its RFC 8785
/// JCS-canonical params. Two calls share a `CallKey` iff their
/// `(tool_name, canonical_params)` pair is equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallKey(String);

impl CallKey {
    /// Borrow the underlying `tool_name + canonical_params` string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 16-byte BLAKE3 prefix of the canonical-JSON tool-result payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResultHash([u8; 16]);

impl ResultHash {
    /// Copy of the 16-byte hash.
    pub fn as_bytes(&self) -> [u8; 16] {
        self.0
    }
}

/// Hash plus canonical-payload byte length computed from one canonical
/// byte buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResultFingerprint {
    hash: ResultHash,
    canonical_len: usize,
}

impl ResultFingerprint {
    /// 16-byte BLAKE3 prefix of the canonical result payload.
    pub fn hash(self) -> ResultHash {
        self.hash
    }

    /// Byte length of the canonical result payload.
    pub fn canonical_len(self) -> usize {
        self.canonical_len
    }
}

/// Shared canonicalization + BLAKE3-16 utility. Stateless — the live
/// conversation loop computes a [`ResultFingerprint`] once per tool
/// result and passes it to both built-in observers.
#[derive(Debug, Default, Clone, Copy)]
pub struct ResultHasher;

impl ResultHasher {
    /// Construct a `ResultHasher`. The type is a ZST today; the
    /// constructor exists so observer scaffolds that store a hasher
    /// instance compile without coupling to `Default`.
    pub fn new() -> Self {
        Self
    }

    /// Compose the `(tool_name, canonical_params)` `CallKey` two
    /// successive calls share when their params are JCS-equivalent.
    pub fn call_key(tool_name: &str, params: &Value) -> Result<CallKey, Error> {
        let canon = canonical_string(params)?;
        let mut buf = String::with_capacity(tool_name.len() + 1 + canon.len());
        buf.push_str(tool_name);
        buf.push('\u{1f}');
        buf.push_str(&canon);
        Ok(CallKey(buf))
    }

    /// Hash and byte length of the canonical JSON serialisation of
    /// `result`, computed from one canonical byte buffer.
    pub fn result_fingerprint(result: &Value) -> Result<ResultFingerprint, Error> {
        let bytes = canonical_bytes(result)?;
        let full = blake3::hash(&bytes);
        let mut prefix = [0u8; 16];
        prefix.copy_from_slice(&full.as_bytes()[..16]);
        Ok(ResultFingerprint {
            hash: ResultHash(prefix),
            canonical_len: bytes.len(),
        })
    }

    /// Parse a tool-result output string and fingerprint the resulting
    /// JSON value, treating non-JSON output as a JSON string payload.
    pub fn output_fingerprint(output: &str) -> Result<ResultFingerprint, Error> {
        let value = parse_output(output);
        Self::result_fingerprint(&value)
    }

    /// BLAKE3-16 of the canonical JSON serialisation of `result`.
    pub fn result_hash(result: &Value) -> Result<ResultHash, Error> {
        Ok(Self::result_fingerprint(result)?.hash())
    }

    /// Byte length of the canonical JSON serialisation of `result`.
    /// Shared so `DuplicateResultObserver`'s `min_bytes` filter and
    /// `bytes_wasted` event payload use the exact same notion of size
    /// the hashing pipeline does.
    pub fn canonical_len(result: &Value) -> Result<usize, Error> {
        Ok(Self::result_fingerprint(result)?.canonical_len())
    }
}

fn parse_output(output: &str) -> Value {
    match serde_json::from_str::<Value>(output) {
        Ok(value) => value,
        Err(_plain_text) => Value::String(output.to_owned()),
    }
}

fn canonical_bytes<T>(value: &T) -> Result<Vec<u8>, Error>
where
    T: serde::Serialize + ?Sized,
{
    serde_jcs::to_vec(value).map_err(Error::from)
}

fn canonical_string(value: &Value) -> Result<String, Error> {
    serde_jcs::to_string(value).map_err(Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    fn hash(value: &Value) -> ResultHash {
        ResultHasher::result_hash(value).expect("canonicalize result")
    }

    fn key(tool_name: &str, params: &Value) -> CallKey {
        ResultHasher::call_key(tool_name, params).expect("canonicalize params")
    }

    #[test]
    fn result_hash_is_blake3_16_byte_prefix_of_canonical_payload() {
        let value = json!({"b": 1, "a": 2});
        let canonical = serde_jcs::to_vec(&value).expect("canonicalize");
        let expected = blake3::hash(&canonical);
        let got = hash(&value);
        assert_eq!(got.as_bytes(), expected.as_bytes()[..16]);
    }

    #[test]
    fn result_hash_is_stable_under_object_key_reordering() {
        let a = json!({"alpha": 1, "beta": [2, 3], "gamma": {"x": true}});
        let b = json!({"gamma": {"x": true}, "beta": [2, 3], "alpha": 1});
        assert_eq!(hash(&a), hash(&b));
    }

    #[test]
    fn result_hash_distinguishes_distinct_payloads() {
        let a = json!({"x": 1});
        let b = json!({"x": 2});
        assert_ne!(hash(&a), hash(&b));
    }

    #[test]
    fn result_hash_preserves_array_order() {
        let a = json!([1, 2, 3]);
        let b = json!([3, 2, 1]);
        assert_ne!(hash(&a), hash(&b));
    }

    #[test]
    fn call_key_is_stable_under_param_key_reordering() {
        let params_a = json!({"foo": 1, "bar": [true, null]});
        let params_b = json!({"bar": [true, null], "foo": 1});
        assert_eq!(key("read_file", &params_a), key("read_file", &params_b));
    }

    #[test]
    fn call_key_differs_when_tool_name_changes() {
        let params = json!({"path": "/tmp/x"});
        assert_ne!(key("read_file", &params), key("write_file", &params));
    }

    #[test]
    fn call_key_differs_when_params_change() {
        let a = json!({"path": "/tmp/a"});
        let b = json!({"path": "/tmp/b"});
        assert_ne!(key("read_file", &a), key("read_file", &b));
    }

    #[test]
    fn call_key_embeds_canonical_params() {
        let key = key("t", &json!({"b": 1, "a": 2}));
        assert!(
            key.as_str().contains("{\"a\":2,\"b\":1}"),
            "expected JCS-sorted params in call key, got {:?}",
            key.as_str(),
        );
    }

    #[test]
    fn call_key_separator_prevents_tool_name_params_collision() {
        let a = key("tool", &json!("name"));
        let b = key("toolname", &json!(""));
        assert_ne!(a, b);
    }

    #[test]
    fn canonical_len_matches_jcs_byte_count() {
        let value = json!({"b": 1, "a": 2});
        let bytes = serde_jcs::to_vec(&value).expect("canonicalize");
        assert_eq!(
            ResultHasher::canonical_len(&value).expect("canonicalize result"),
            bytes.len(),
        );
    }

    #[test]
    fn jcs_failure_is_propagated_as_typed_error() {
        struct RefusesSerialization;

        impl serde::Serialize for RefusesSerialization {
            fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                Err(serde::ser::Error::custom("fixture refuses serialization"))
            }
        }

        let error = canonical_bytes(&RefusesSerialization).expect_err("JCS must fail");
        assert!(matches!(error, Error::Canonicalization(_)));
    }

    #[test]
    fn result_hash_handles_nested_structures() {
        let value = json!({
            "outer": {
                "inner": [
                    {"k": "v"},
                    {"k": "w"},
                ],
            },
            "flag": true,
        });
        let twin = json!({
            "flag": true,
            "outer": {
                "inner": [
                    {"k": "v"},
                    {"k": "w"},
                ],
            },
        });
        assert_eq!(hash(&value), hash(&twin));
    }
}
