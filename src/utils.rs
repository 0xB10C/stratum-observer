use bitcoin::blockdata::script::ScriptBuf;
use std::fmt::Write;

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

pub fn encode_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        write!(&mut s, "{:02x}", b).unwrap();
    }
    s
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
    fn test_extract_coinbase_string() {
        let test_cases = vec![
            (   // mainnet 82f3748ef4ffc2acfefbbc682adf34532eef41e13c4819085a8af611a8f118fa
                "03a4120d04b5a2bc667c204d41524120506f6f6c207c204d61646520696e2055534120f09f87baf09f87b8207c202876303331393234293c39dbd326a3c601527b7112d4f11f911356a5e9450000000000ffffffff",
                "f| MARA Pool | Made in USA  | (v031924)<9"
            ),
            (   // mainnet 9603a850d3da9f230d36d03c83eb402c18fae9754cdb528a242c96dac0187538
                "03a3120d1b4d696e656420627920416e74506f6f6c3837349f00010293f49debfabe6d6d7ce2d695ffb032041ea85db181f3aea78cf47946d5e610fe32292d95db1eca551000000000000000540c00005737000000000000",
                "Mined by AntPool874"
            ),
            (   // mainnet 4808de30cf5ad96fbce89f7efabcebf8222f098c51926a282472292ac9291d1d
                "0390120d182f5669614254432f4d696e6564206279206173646c31372f2cfabe6d6d038689c77c851fb85eef5e10eade9efb405e21994412c978983b2e71881f471610000000000000001046b1dc05df138865a39bb08f551f080000000000",
                "/ViaBTC/Mined by asdl17/,"
            ),
            (   // mainnet dbb5ac4b963babbaa3a7c85ef234959a702d583c09f1a609ad7f5b80cc0c064a
                "031c120d048867bb662f466f756e6472792055534120506f6f6c202364726f70676f6c642f4892b78e0000b3b25d010000",
                "f/Foundry USA Pool #dropgold/H"
            ),
        ];

        for (hex, result) in test_cases {
            assert_eq!(
                extract_coinbase_string(&ScriptBuf::from_hex(hex).unwrap()),
                result,
            );
        }
    }

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
