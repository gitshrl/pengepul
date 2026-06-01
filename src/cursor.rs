pub(crate) fn encode_varint(mut value: u32, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push(((value & 0x7f) as u8) | 0x80);
        value >>= 7;
    }
    out.push((value & 0xff) as u8);
}

pub(crate) fn encode_varint_field(field: u32, value: u32, out: &mut Vec<u8>) {
    encode_varint(field << 3, out); // wire type 0
    encode_varint(value, out);
}

pub(crate) fn encode_bytes_field(field: u32, payload: &[u8], out: &mut Vec<u8>) {
    encode_varint((field << 3) | 2, out); // wire type 2
    encode_varint(u32::try_from(payload.len()).unwrap_or(u32::MAX), out);
    out.extend_from_slice(payload);
}

pub(crate) fn connect_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(0);
    frame.extend_from_slice(&u32::try_from(payload.len()).unwrap_or(u32::MAX).to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

#[derive(Debug)]
pub(crate) struct RawField {
    pub field: u32,
    pub wire: u8,
    pub bytes: Option<Vec<u8>>,
    pub varint: Option<u64>,
}

fn decode_varint(data: &[u8], pos: usize) -> (u64, usize) {
    let (mut value, mut shift, mut p) = (0u64, 0u32, pos);
    while p < data.len() {
        let b = data[p];
        p += 1;
        value |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    (value, p)
}

pub(crate) fn parse_fields(data: &[u8]) -> Vec<RawField> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let (tag, after) = decode_varint(data, pos);
        if after <= pos {
            break;
        }
        pos = after;
        let field = u32::try_from(tag >> 3).unwrap_or(0);
        let wire = (tag & 7) as u8;
        match wire {
            0 => {
                let (v, p) = decode_varint(data, pos);
                out.push(RawField {
                    field,
                    wire,
                    bytes: None,
                    varint: Some(v),
                });
                pos = p;
            }
            2 => {
                let (len, after_len) = decode_varint(data, pos);
                pos = after_len;
                let len = usize::try_from(len).unwrap_or(usize::MAX);
                if pos + len > data.len() {
                    break;
                }
                out.push(RawField {
                    field,
                    wire,
                    bytes: Some(data[pos..pos + len].to_vec()),
                    varint: None,
                });
                pos += len;
            }
            1 => pos += 8,
            5 => pos += 4,
            _ => break,
        }
    }
    out
}

pub(crate) fn field_bytes(fields: &[RawField], field: u32) -> Option<&[u8]> {
    fields
        .iter()
        .find(|f| f.field == field && f.wire == 2)
        .and_then(|f| f.bytes.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_and_field_encoding_match_known_vectors() {
        let mut v = Vec::new();
        encode_varint(300, &mut v);
        assert_eq!(v, vec![0xac, 0x02]);
        let mut b = Vec::new();
        encode_bytes_field(1, b"hi", &mut b);
        assert_eq!(b, vec![0x0a, 0x02, b'h', b'i']);
        let mut n = Vec::new();
        encode_varint_field(2, 1, &mut n);
        assert_eq!(n, vec![0x10, 0x01]);
    }

    #[test]
    fn parse_fields_round_trips_a_bytes_field() {
        let mut buf = Vec::new();
        encode_bytes_field(1, b"hello", &mut buf);
        encode_varint_field(2, 7, &mut buf);
        let fields = parse_fields(&buf);
        assert_eq!(field_bytes(&fields, 1).unwrap(), b"hello");
        assert_eq!(fields.iter().find(|f| f.field == 2).unwrap().varint, Some(7));
    }
}
