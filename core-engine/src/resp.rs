const MAX_ARRAY_DEPTH: usize = 16;
const MAX_ARRAY_ELEMENTS: usize = 1_000_000;
/// Largest bulk string the parser will accept, in bytes. Public because
/// `CONFIG GET proto-max-bulk-len` has to report the limit actually enforced
/// rather than a second copy of the number.
pub const MAX_BULK_STRING_BYTES: usize = 64 * 1024 * 1024; // 64 MB
const MAX_TOTAL_MESSAGE_BYTES: usize = 64 * 1024 * 1024; // 64 MB total per message

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    SimpleString(String),
    Error(String),
    Integer(i64),
    BulkString(Option<Vec<u8>>),
    Array(Option<Vec<Value>>),
    /// RESP3 Push frame (`>N\r\n...`). Used for server-initiated out-of-band messages
    /// (mutation fan-out, pub/sub) on the WebSocket channel. Never sent as a command response.
    Push(Vec<Value>),
    /// RESP3 Map (`%N\r\n` followed by `N` key/value pairs).
    ///
    /// Only `HELLO 3` replies with one today. A RESP2 connection must never be
    /// sent a map — the type does not exist in RESP2 and the client will fail
    /// to parse it — so the caller picks the shape from the negotiated version.
    Map(Vec<(Value, Value)>),
}

impl Value {
    /// Serializes the Value back into RESP format.
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.serialize_into(&mut buf);
        buf
    }

    /// Appends the RESP encoding of `self` to `out`. Unlike `serialize`, this
    /// allocates nothing of its own — array elements are encoded in place —
    /// so hot paths can reuse one output buffer across responses.
    pub fn serialize_into(&self, out: &mut Vec<u8>) {
        use std::io::Write;
        match self {
            Value::SimpleString(s) => {
                out.push(b'+');
                out.extend_from_slice(s.as_bytes());
                out.extend_from_slice(b"\r\n");
            }
            Value::Error(s) => {
                out.push(b'-');
                out.extend_from_slice(s.as_bytes());
                out.extend_from_slice(b"\r\n");
            }
            Value::Integer(i) => {
                let _ = write!(out, ":{}\r\n", i);
            }
            Value::BulkString(None) => {
                out.extend_from_slice(b"$-1\r\n");
            }
            Value::BulkString(Some(data)) => {
                let _ = write!(out, "${}\r\n", data.len());
                out.extend_from_slice(data);
                out.extend_from_slice(b"\r\n");
            }
            Value::Array(None) => {
                out.extend_from_slice(b"*-1\r\n");
            }
            Value::Array(Some(arr)) => {
                let _ = write!(out, "*{}\r\n", arr.len());
                for v in arr {
                    v.serialize_into(out);
                }
            }
            Value::Push(arr) => {
                let _ = write!(out, ">{}\r\n", arr.len());
                for v in arr {
                    v.serialize_into(out);
                }
            }
            Value::Map(pairs) => {
                // The header counts *pairs*, not elements, so a 3-entry map is
                // `%3` followed by six values.
                let _ = write!(out, "%{}\r\n", pairs.len());
                for (k, v) in pairs {
                    k.serialize_into(out);
                    v.serialize_into(out);
                }
            }
        }
    }

    /// Parses a byte slice into a RESP Value, returning the Value and the number of bytes consumed.
    pub fn parse(buffer: &[u8]) -> Result<(Value, usize), String> {
        Self::parse_inner(buffer, 0)
    }

    fn parse_inner(buffer: &[u8], depth: usize) -> Result<(Value, usize), String> {
        if buffer.is_empty() {
            return Err("Incomplete".to_string());
        }
        match buffer[0] {
            b'+' => Self::parse_simple_string(buffer),
            b'-' => Self::parse_error(buffer),
            b':' => Self::parse_integer(buffer),
            b'$' => Self::parse_bulk_string(buffer),
            b'*' => Self::parse_array(buffer, depth),
            b'>' => Self::parse_push(buffer, depth),
            b'%' => Self::parse_map(buffer, depth),
            _ => Err("Invalid RESP type".to_string()),
        }
    }

    fn read_until_crlf(buffer: &[u8]) -> Option<(&[u8], usize)> {
        for i in 0..buffer.len().saturating_sub(1) {
            if buffer[i] == b'\r' && buffer[i + 1] == b'\n' {
                return Some((&buffer[1..i], i + 2));
            }
        }
        None
    }

    fn parse_simple_string(buffer: &[u8]) -> Result<(Value, usize), String> {
        match Self::read_until_crlf(buffer) {
            Some((data, len)) => Ok((
                Value::SimpleString(String::from_utf8_lossy(data).into_owned()),
                len,
            )),
            None => Err("Incomplete".to_string()),
        }
    }

    fn parse_error(buffer: &[u8]) -> Result<(Value, usize), String> {
        match Self::read_until_crlf(buffer) {
            Some((data, len)) => Ok((
                Value::Error(String::from_utf8_lossy(data).into_owned()),
                len,
            )),
            None => Err("Incomplete".to_string()),
        }
    }

    fn parse_integer(buffer: &[u8]) -> Result<(Value, usize), String> {
        match Self::read_until_crlf(buffer) {
            Some((data, len)) => {
                let s = String::from_utf8_lossy(data);
                let i = s
                    .parse::<i64>()
                    .map_err(|_| "Invalid integer format".to_string())?;
                Ok((Value::Integer(i), len))
            }
            None => Err("Incomplete".to_string()),
        }
    }

    fn parse_bulk_string(buffer: &[u8]) -> Result<(Value, usize), String> {
        match Self::read_until_crlf(buffer) {
            Some((data, head_len)) => {
                let s = String::from_utf8_lossy(data);
                let length: i64 = s
                    .parse()
                    .map_err(|_| "Invalid bulk string length".to_string())?;

                if length == -1 {
                    return Ok((Value::BulkString(None), head_len));
                }
                if length < 0 {
                    return Err("Invalid bulk string length".to_string());
                }

                let length = length as usize;
                if length > MAX_BULK_STRING_BYTES {
                    return Err(format!(
                        "ERR bulk string too large ({} > {} bytes)",
                        length, MAX_BULK_STRING_BYTES
                    ));
                }
                let end = head_len + length + 2; // +2 for trailing CRLF
                if buffer.len() < end {
                    return Err("Incomplete".to_string());
                }

                let str_data = buffer[head_len..head_len + length].to_vec();
                Ok((Value::BulkString(Some(str_data)), end))
            }
            None => Err("Incomplete".to_string()),
        }
    }

    fn parse_push(buffer: &[u8], depth: usize) -> Result<(Value, usize), String> {
        if depth >= MAX_ARRAY_DEPTH {
            return Err("ERR max nesting depth exceeded".to_string());
        }
        match Self::read_until_crlf(buffer) {
            Some((data, mut offset)) => {
                let s = String::from_utf8_lossy(data);
                let count: u64 = s
                    .parse()
                    .map_err(|_| "Invalid push frame length".to_string())?;
                if count as usize > MAX_ARRAY_ELEMENTS {
                    return Err(format!(
                        "ERR push frame too large ({} > {} elements)",
                        count, MAX_ARRAY_ELEMENTS
                    ));
                }
                let mut arr = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    let (val, len) = Self::parse_inner(&buffer[offset..], depth + 1)?;
                    arr.push(val);
                    offset += len;
                    if offset > MAX_TOTAL_MESSAGE_BYTES {
                        return Err("ERR message too large".to_string());
                    }
                }
                Ok((Value::Push(arr), offset))
            }
            None => Err("Incomplete".to_string()),
        }
    }

    fn parse_map(buffer: &[u8], depth: usize) -> Result<(Value, usize), String> {
        if depth >= MAX_ARRAY_DEPTH {
            return Err("ERR max nesting depth exceeded".to_string());
        }
        match Self::read_until_crlf(buffer) {
            Some((data, mut offset)) => {
                let s = String::from_utf8_lossy(data);
                let count: u64 = s.parse().map_err(|_| "Invalid map length".to_string())?;
                // Each pair is two values, so the element budget is halved.
                if count as usize > MAX_ARRAY_ELEMENTS / 2 {
                    return Err(format!(
                        "ERR map too large ({} > {} pairs)",
                        count,
                        MAX_ARRAY_ELEMENTS / 2
                    ));
                }
                let mut pairs = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    let (k, klen) = Self::parse_inner(&buffer[offset..], depth + 1)?;
                    offset += klen;
                    let (v, vlen) = Self::parse_inner(&buffer[offset..], depth + 1)?;
                    offset += vlen;
                    pairs.push((k, v));
                    if offset > MAX_TOTAL_MESSAGE_BYTES {
                        return Err("ERR message too large".to_string());
                    }
                }
                Ok((Value::Map(pairs), offset))
            }
            None => Err("Incomplete".to_string()),
        }
    }

    fn parse_array(buffer: &[u8], depth: usize) -> Result<(Value, usize), String> {
        if depth >= MAX_ARRAY_DEPTH {
            return Err("ERR max nesting depth exceeded".to_string());
        }

        match Self::read_until_crlf(buffer) {
            Some((data, mut offset)) => {
                let s = String::from_utf8_lossy(data);
                let count: i64 = s.parse().map_err(|_| "Invalid array length".to_string())?;

                if count == -1 {
                    return Ok((Value::Array(None), offset));
                }
                if count < 0 {
                    return Err("Invalid array length".to_string());
                }
                if count as usize > MAX_ARRAY_ELEMENTS {
                    return Err(format!(
                        "ERR array too large ({} > {} elements)",
                        count, MAX_ARRAY_ELEMENTS
                    ));
                }

                let mut arr = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    let (val, len) = Self::parse_inner(&buffer[offset..], depth + 1)?;
                    arr.push(val);
                    offset += len;
                    if offset > MAX_TOTAL_MESSAGE_BYTES {
                        return Err("ERR message too large".to_string());
                    }
                }

                Ok((Value::Array(Some(arr)), offset))
            }
            None => Err("Incomplete".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(v: &Value) {
        let bytes = v.serialize();
        let (parsed, consumed) = Value::parse(&bytes).expect("parse failed");
        assert_eq!(&parsed, v);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn simple_string_round_trip() {
        round_trip(&Value::SimpleString("PONG".to_string()));
        round_trip(&Value::SimpleString("OK".to_string()));
        round_trip(&Value::SimpleString(String::new()));
    }

    #[test]
    fn error_round_trip() {
        round_trip(&Value::Error("ERR unknown command".to_string()));
    }

    #[test]
    fn integer_round_trip() {
        round_trip(&Value::Integer(0));
        round_trip(&Value::Integer(42));
        round_trip(&Value::Integer(-1));
        round_trip(&Value::Integer(i64::MAX));
        round_trip(&Value::Integer(i64::MIN));
    }

    #[test]
    fn bulk_string_round_trip() {
        round_trip(&Value::BulkString(Some(b"hello".to_vec())));
        round_trip(&Value::BulkString(Some(b"hello world".to_vec())));
        round_trip(&Value::BulkString(Some(vec![]))); // empty bulk string
        round_trip(&Value::BulkString(None)); // null bulk string
    }

    #[test]
    fn bulk_string_with_crlf_inside() {
        // Values containing \r\n must survive round-trip via the length-prefixed format
        round_trip(&Value::BulkString(Some(b"foo\r\nbar".to_vec())));
    }

    #[test]
    fn array_round_trip() {
        round_trip(&Value::Array(None));
        round_trip(&Value::Array(Some(vec![])));
        round_trip(&Value::Array(Some(vec![
            Value::BulkString(Some(b"SET".to_vec())),
            Value::BulkString(Some(b"key".to_vec())),
            Value::BulkString(Some(b"value".to_vec())),
        ])));
    }

    #[test]
    fn nested_array_round_trip() {
        let inner = Value::Array(Some(vec![Value::Integer(1), Value::Integer(2)]));
        round_trip(&Value::Array(Some(vec![inner])));
    }

    #[test]
    fn incomplete_returns_error() {
        assert_eq!(Value::parse(b""), Err("Incomplete".to_string()));
        assert_eq!(Value::parse(b"+OK"), Err("Incomplete".to_string())); // missing \r\n
        assert_eq!(Value::parse(b"$5\r\nhell"), Err("Incomplete".to_string())); // truncated bulk
        assert_eq!(
            Value::parse(b"*2\r\n+OK\r\n"),
            Err("Incomplete".to_string())
        ); // array missing 2nd element
    }

    #[test]
    fn invalid_resp_type_byte() {
        assert_eq!(
            Value::parse(b"!garbage"),
            Err("Invalid RESP type".to_string())
        );
    }

    #[test]
    fn depth_limit_exceeded() {
        // Build a deeply nested array: *1\r\n*1\r\n... repeated MAX_ARRAY_DEPTH+1 times
        let mut payload = String::new();
        for _ in 0..=MAX_ARRAY_DEPTH {
            payload.push_str("*1\r\n");
        }
        payload.push_str("+leaf\r\n");
        let result = Value::parse(payload.as_bytes());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("max nesting depth"));
    }

    #[test]
    fn parse_consumes_exactly_right_bytes() {
        // Two concatenated simple strings; parse should stop after the first
        let input = b"+OK\r\n+NEXT\r\n";
        let (val, consumed) = Value::parse(input).unwrap();
        assert_eq!(val, Value::SimpleString("OK".to_string()));
        assert_eq!(consumed, 5); // "+OK\r\n" = 5 bytes
    }

    #[test]
    fn push_round_trip() {
        round_trip(&Value::Map(vec![]));
        round_trip(&Value::Map(vec![(
            Value::BulkString(Some(b"proto".to_vec())),
            Value::Integer(3),
        )]));
        round_trip(&Value::Map(vec![
            (
                Value::BulkString(Some(b"server".to_vec())),
                Value::BulkString(Some(b"recached".to_vec())),
            ),
            (
                Value::BulkString(Some(b"modules".to_vec())),
                Value::Array(Some(vec![])),
            ),
        ]));
        round_trip(&Value::Push(vec![]));
        round_trip(&Value::Push(vec![
            Value::BulkString(Some(b"SET".to_vec())),
            Value::BulkString(Some(b"theme".to_vec())),
            Value::BulkString(Some(b"dark".to_vec())),
        ]));
        round_trip(&Value::Push(vec![
            Value::BulkString(Some(b"message".to_vec())),
            Value::BulkString(Some(b"alerts".to_vec())),
            Value::BulkString(Some(b"hello".to_vec())),
        ]));
    }

    #[test]
    fn map_header_counts_pairs_not_elements() {
        // `%N` means N key/value *pairs* — 2N values follow. Emitting the
        // element count instead would desynchronise every downstream parser.
        let m = Value::Map(vec![
            (Value::BulkString(Some(b"a".to_vec())), Value::Integer(1)),
            (Value::BulkString(Some(b"b".to_vec())), Value::Integer(2)),
        ]);
        let bytes = m.serialize();
        assert!(
            bytes.starts_with(b"%2\r\n"),
            "got {:?}",
            String::from_utf8_lossy(&bytes)
        );
        let (parsed, n) = Value::parse(&bytes).unwrap();
        assert_eq!(parsed, m);
        assert_eq!(n, bytes.len(), "must consume exactly the frame");
    }

    #[test]
    fn map_rejects_a_malformed_length() {
        assert!(Value::parse(b"%x\r\n").is_err());
        assert!(Value::parse(b"%-1\r\n").is_err());
    }

    #[test]
    fn truncated_map_is_incomplete_not_an_error() {
        // A partial frame must be retried once more bytes arrive, not rejected.
        let full = Value::Map(vec![(
            Value::BulkString(Some(b"k".to_vec())),
            Value::Integer(7),
        )])
        .serialize();
        for cut in 1..full.len() {
            assert_eq!(
                Value::parse(&full[..cut]),
                Err("Incomplete".to_string()),
                "prefix of length {cut} should be incomplete"
            );
        }
    }

    #[test]
    fn push_prefix_distinct_from_array() {
        let push = Value::Push(vec![Value::BulkString(Some(b"x".to_vec()))]).serialize();
        let arr = Value::Array(Some(vec![Value::BulkString(Some(b"x".to_vec()))])).serialize();
        assert_ne!(push, arr);
        assert!(push.starts_with(b">"));
        assert!(arr.starts_with(b"*"));
    }

    #[test]
    fn null_array_vs_empty_array() {
        let null = Value::Array(None).serialize();
        let empty = Value::Array(Some(vec![])).serialize();
        assert_ne!(null, empty);
        assert_eq!(null, b"*-1\r\n");
        assert_eq!(empty, b"*0\r\n");
    }
}

#[cfg(test)]
mod parser_edge_tests {
    use super::*;

    /// Every prefix of a valid frame must report `Incomplete` rather than
    /// parsing garbage — this is the property that makes streaming reads safe
    /// when TCP hands us a partial frame.
    fn every_prefix_is_incomplete(frame: &[u8]) {
        for cut in 1..frame.len() {
            let err = Value::parse(&frame[..cut]).expect_err(&format!(
                "prefix of {cut} bytes should not parse: {frame:?}"
            ));
            assert!(
                err.contains("Incomplete") || err.contains("too large") || err.contains("Invalid"),
                "prefix {cut}: unexpected error {err}"
            );
        }
    }

    #[test]
    fn parses_error_frames() {
        let (v, n) = Value::parse(b"-ERR something broke\r\n").unwrap();
        assert_eq!(v, Value::Error("ERR something broke".to_string()));
        assert_eq!(n, 22);
    }

    #[test]
    fn parses_integer_frames_including_negatives() {
        assert_eq!(Value::parse(b":42\r\n").unwrap().0, Value::Integer(42));
        assert_eq!(Value::parse(b":-7\r\n").unwrap().0, Value::Integer(-7));
        assert_eq!(Value::parse(b":0\r\n").unwrap().0, Value::Integer(0));
    }

    #[test]
    fn rejects_malformed_integer() {
        let err = Value::parse(b":notanumber\r\n").unwrap_err();
        assert!(err.contains("Invalid integer"), "got {err}");
    }

    #[test]
    fn partial_frames_report_incomplete() {
        every_prefix_is_incomplete(b"-ERR broke\r\n");
        every_prefix_is_incomplete(b":123\r\n");
        every_prefix_is_incomplete(b"$5\r\nhello\r\n");
        every_prefix_is_incomplete(b"*2\r\n$1\r\na\r\n$1\r\nb\r\n");
    }

    #[test]
    fn parses_push_frames() {
        let frame = b">2\r\n$7\r\nmessage\r\n$2\r\nhi\r\n";
        let (v, n) = Value::parse(frame).unwrap();
        assert_eq!(
            v,
            Value::Push(vec![
                Value::BulkString(Some(b"message".to_vec())),
                Value::BulkString(Some(b"hi".to_vec())),
            ])
        );
        assert_eq!(n, frame.len());
    }

    #[test]
    fn push_frame_round_trips() {
        let v = Value::Push(vec![
            Value::SimpleString("keychange".into()),
            Value::Integer(1),
        ]);
        let bytes = v.serialize();
        assert_eq!(Value::parse(&bytes).unwrap().0, v);
    }

    #[test]
    fn rejects_malformed_push_length() {
        let err = Value::parse(b">abc\r\n").unwrap_err();
        assert!(err.contains("Invalid push frame length"), "got {err}");
    }

    #[test]
    fn rejects_push_frame_with_too_many_elements() {
        // Declared count over the element cap must be refused on the header
        // alone, before any allocation proportional to it.
        let frame = format!(">{}\r\n", MAX_ARRAY_ELEMENTS + 1);
        let err = Value::parse(frame.as_bytes()).unwrap_err();
        assert!(err.contains("push frame too large"), "got {err}");
    }

    #[test]
    fn rejects_array_frame_with_too_many_elements() {
        let frame = format!("*{}\r\n", MAX_ARRAY_ELEMENTS + 1);
        let err = Value::parse(frame.as_bytes()).unwrap_err();
        assert!(err.contains("too large"), "got {err}");
    }

    #[test]
    fn rejects_nesting_beyond_the_depth_limit() {
        // A hostile client can otherwise drive unbounded recursion with a few
        // bytes per level.
        let mut frame = Vec::new();
        for _ in 0..(MAX_ARRAY_DEPTH + 2) {
            frame.extend_from_slice(b"*1\r\n");
        }
        frame.extend_from_slice(b"$1\r\na\r\n");
        let err = Value::parse(&frame).unwrap_err();
        assert!(err.contains("nesting depth"), "got {err}");
    }

    #[test]
    fn nesting_within_the_limit_still_parses() {
        let mut frame = Vec::new();
        for _ in 0..(MAX_ARRAY_DEPTH - 2) {
            frame.extend_from_slice(b"*1\r\n");
        }
        frame.extend_from_slice(b"$1\r\na\r\n");
        assert!(Value::parse(&frame).is_ok());
    }

    #[test]
    fn rejects_oversized_bulk_string_header() {
        let frame = format!("${}\r\n", MAX_BULK_STRING_BYTES + 1);
        let err = Value::parse(frame.as_bytes()).unwrap_err();
        assert!(err.contains("too large"), "got {err}");
    }

    #[test]
    fn rejects_unknown_type_byte() {
        let err = Value::parse(b"%1\r\n").unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn empty_buffer_is_incomplete() {
        assert!(Value::parse(b"").is_err());
    }

    #[test]
    fn null_array_and_null_bulk_round_trip() {
        assert_eq!(Value::parse(b"*-1\r\n").unwrap().0, Value::Array(None));
        assert_eq!(Value::parse(b"$-1\r\n").unwrap().0, Value::BulkString(None));
    }

    #[test]
    fn parse_reports_bytes_consumed_so_pipelines_advance() {
        // Two frames back to back: the parser must consume exactly the first.
        let buf = b":1\r\n:2\r\n";
        let (v1, n1) = Value::parse(buf).unwrap();
        assert_eq!(v1, Value::Integer(1));
        assert_eq!(n1, 4);
        let (v2, n2) = Value::parse(&buf[n1..]).unwrap();
        assert_eq!(v2, Value::Integer(2));
        assert_eq!(n2, 4);
    }
}
