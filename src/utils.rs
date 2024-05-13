use bitcoin::blockdata::script::ScriptBuf;

pub fn decode_hex(s: &str) -> Result<Vec<u8>, core::num::ParseIntError> {
    let s = match s.strip_prefix("0x") {
        Some(s) => s,
        None => s,
    };
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
        .collect()
}

/// Extract the block height from a coinbase transactions input script as defined by BIP34.
/// If the script does not start with a parsable BIP34 block height, None is returned.
pub fn bip34_coinbase_block_height(script: &ScriptBuf) -> Option<u32> {
    if script.is_empty() {
        return None;
    }
    let bytes = script.as_bytes();
    let length: usize = bytes[0].into();
    if length > 4 || 1 + length > script.len() {
        return None;
    }
    let mut array: [u8; 4] = [0; 4];
    for (idx, b) in bytes[1..=(length as usize)].iter().enumerate() {
        array[idx] = *b;
    }
    Some(u32::from_le_bytes(array))
}

pub fn extract_coinbase_string(script: &ScriptBuf) -> String {
    let mut coinbase_string = String::new();
    let mut buffer = String::new();
    for b in script.clone().into_bytes() {
        if b >= 32 && b <= 126 {
            buffer.push(b as char);
        } else {
            if buffer.len() >= 6 {
                coinbase_string.push_str(&buffer);
            }
            buffer.clear();
        }
    }
    coinbase_string
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_bip34_coinbase_block_height() {
        let test_cases = vec![
            ("07", None),
            ("071234", None),
            ("051234567890", None),
            ("01", None),
            ("0211", None),
            ("031122", None),
            ("04112233", None),
            ("00", Some(0)),
            ("0164", Some(100)),
            ("02e803", Some(1000)),
            ("03ac9b0c", Some(826284)),
            ("03e19b0c", Some(826337)),
            ("03e29b0c", Some(826338)),
            ("0344ef2a", Some(2813764)),
            ("0400000001", Some(16777216)),
        ];

        for (hex, result) in test_cases {
            assert_eq!(
                bip34_coinbase_block_height(&ScriptBuf::from_hex(hex).unwrap()),
                result,
            );
        }
    }

    #[test]
    fn test_decode_hex() {
        let test_vec = vec![222, 173, 190, 239];
        let hex_og = "deadbeef";
        let hex_og_w_prefix = "0xdeadbeef";
        // decode hex strings
        let decoded = decode_hex(hex_og).unwrap();
        let decoded_w_prefix = decode_hex(hex_og_w_prefix).unwrap();

        assert_eq!(&decoded, &test_vec, "Hex not decoded correctly");
        assert_eq!(
            &decoded_w_prefix, &test_vec,
            "Hex w/ prefix not decoded correctly"
        );
    }
}
