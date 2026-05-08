const MAX_ARRAY_DEPTH: usize = 16;
const MAX_ARRAY_ELEMENTS: usize = 1_000_000;
const MAX_BULK_STRING_BYTES: usize = 64 * 1024 * 1024; // 64 MB

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    SimpleString(String),
    Error(String),
    Integer(i64),
    BulkString(Option<Vec<u8>>),
    Array(Option<Vec<Value>>),
}

impl Value {
    /// Serializes the Value back into RESP format.
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        match self {
            Value::SimpleString(s) => {
                buf.extend_from_slice(b"+");
                buf.extend_from_slice(s.as_bytes());
                buf.extend_from_slice(b"\r\n");
            }
            Value::Error(s) => {
                buf.extend_from_slice(b"-");
                buf.extend_from_slice(s.as_bytes());
                buf.extend_from_slice(b"\r\n");
            }
            Value::Integer(i) => {
                buf.extend_from_slice(format!(":{}\r\n", i).as_bytes());
            }
            Value::BulkString(None) => {
                buf.extend_from_slice(b"$-1\r\n");
            }
            Value::BulkString(Some(data)) => {
                buf.extend_from_slice(format!("${}\r\n", data.len()).as_bytes());
                buf.extend_from_slice(data);
                buf.extend_from_slice(b"\r\n");
            }
            Value::Array(None) => {
                buf.extend_from_slice(b"*-1\r\n");
            }
            Value::Array(Some(arr)) => {
                buf.extend_from_slice(format!("*{}\r\n", arr.len()).as_bytes());
                for v in arr {
                    buf.extend_from_slice(&v.serialize());
                }
            }
        }
        buf
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
    fn null_array_vs_empty_array() {
        let null = Value::Array(None).serialize();
        let empty = Value::Array(Some(vec![])).serialize();
        assert_ne!(null, empty);
        assert_eq!(null, b"*-1\r\n");
        assert_eq!(empty, b"*0\r\n");
    }
}
