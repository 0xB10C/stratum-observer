use bitcoin::blockdata::script::ScriptBuf;
use core::num::ParseIntError;
use std::fmt;
use std::fmt::Write;

/// Reasons why a string can't be decoded into bytes by [decode_hex].
#[derive(Debug, PartialEq, Eq)]
pub enum HexDecodeError {
    /// The string holds an odd number of hex digits, so the last byte is incomplete.
    OddLength,
    /// The string holds something that isn't a hex digit.
    InvalidDigit(ParseIntError),
}

impl fmt::Display for HexDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HexDecodeError::OddLength => write!(f, "hex string has an odd number of digits"),
            HexDecodeError::InvalidDigit(e) => write!(f, "hex string has an invalid digit: {}", e),
        }
    }
}

impl std::error::Error for HexDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            HexDecodeError::OddLength => None,
            HexDecodeError::InvalidDigit(e) => Some(e),
        }
    }
}

/// Decode a hex string, with or without a leading `0x`, into bytes.
pub fn decode_hex(s: &str) -> Result<Vec<u8>, HexDecodeError> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let digits = s.as_bytes();
    if digits.len() % 2 != 0 {
        return Err(HexDecodeError::OddLength);
    }
    digits
        .chunks(2)
        .map(|pair| {
            // Work on bytes rather than on characters, as a string can hold
            // characters that are more than one byte long. Such a pair of bytes
            // can't be valid hex anyway, so we hand it to the parser as a pair
            // that it rejects.
            let pair = std::str::from_utf8(pair).unwrap_or("zz");
            u8::from_str_radix(pair, 16).map_err(HexDecodeError::InvalidDigit)
        })
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

/// Minimum number of printable characters in a row for them to be considered
/// part of the coinbase tag.
const MIN_COINBASE_STRING_RUN: usize = 6;

pub fn extract_coinbase_string(script: &ScriptBuf) -> String {
    let mut coinbase_string = String::new();
    let mut buffer = String::new();
    for b in script.as_bytes() {
        if (32..=126).contains(b) {
            buffer.push(*b as char);
        } else {
            if buffer.len() >= MIN_COINBASE_STRING_RUN {
                coinbase_string.push_str(&buffer);
            }
            buffer.clear();
        }
    }
    // The script can end in the middle of a run of printable characters. Keep
    // that last run too, instead of dropping it.
    if buffer.len() >= MIN_COINBASE_STRING_RUN {
        coinbase_string.push_str(&buffer);
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
    fn test_extract_coinbase_string_at_end_of_script() {
        // A BIP34 height push followed by a tag that reaches the end of the
        // script. Nothing follows the tag, so it has to be returned anyway.
        let script = ScriptBuf::from_hex("03a4120d2f466f756e6472792055534120506f6f6c2f").unwrap();
        assert_eq!(extract_coinbase_string(&script), "/Foundry USA Pool/");
    }

    #[test]
    fn test_extract_coinbase_string_drops_short_runs() {
        // Runs of less than six printable characters are dropped, both in the
        // middle of the script and at its end.
        let five = ScriptBuf::from_hex("00414243444500").unwrap();
        assert_eq!(extract_coinbase_string(&five), "");
        let five_at_end = ScriptBuf::from_hex("004142434445").unwrap();
        assert_eq!(extract_coinbase_string(&five_at_end), "");
        // Six characters are long enough.
        let six = ScriptBuf::from_hex("00414243444546").unwrap();
        assert_eq!(extract_coinbase_string(&six), "ABCDEF");
    }

    #[test]
    fn test_extract_coinbase_string_concatenates_runs() {
        // Two long runs separated by non printable bytes end up next to each
        // other, without a separator in between.
        let script = ScriptBuf::from_hex("414243444546ff00ff4748494a4b4c").unwrap();
        assert_eq!(extract_coinbase_string(&script), "ABCDEFGHIJKL");
    }

    #[test]
    fn test_extract_coinbase_string_empty_script() {
        assert_eq!(extract_coinbase_string(&ScriptBuf::new()), "");
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
    fn test_bip34_coinbase_block_height_edge_cases() {
        // An empty script has no height in it.
        assert_eq!(bip34_coinbase_block_height(&ScriptBuf::new()), None);

        // The height is pushed with one up to four bytes, in little endian
        // byte order.
        let cases = vec![
            ("0101", Some(1)),
            ("02ffff", Some(65535)),
            ("03ffffff", Some(16777215)),
            ("0411223344", Some(0x44332211)),
        ];
        for (hex, height) in cases {
            assert_eq!(
                bip34_coinbase_block_height(&ScriptBuf::from_hex(hex).unwrap()),
                height,
                "height of {}",
                hex
            );
        }

        // A push that claims more bytes than the script holds is rejected
        // rather than read past the end of the script.
        for hex in ["01", "0211", "031122", "04112233", "0411"] {
            assert_eq!(
                bip34_coinbase_block_height(&ScriptBuf::from_hex(hex).unwrap()),
                None,
                "push longer than script: {}",
                hex
            );
        }

        // Five bytes is more than a height needs, so it isn't a height.
        assert_eq!(
            bip34_coinbase_block_height(&ScriptBuf::from_hex("051234567890").unwrap()),
            None
        );

        // BIP34 heights are script numbers, where the top bit of the last byte
        // is the sign. A push of 0x80 is therefore "negative zero", and one of
        // 0x81 is -1. Heights are never negative in practice, and we read the
        // bytes as a plain unsigned number, so this is what comes out.
        assert_eq!(
            bip34_coinbase_block_height(&ScriptBuf::from_hex("0180").unwrap()),
            Some(128)
        );
        assert_eq!(
            bip34_coinbase_block_height(&ScriptBuf::from_hex("0181").unwrap()),
            Some(129)
        );

        // BIP34 also allows the heights 1 to 16 to be pushed with the single
        // byte opcodes OP_1 (0x51) to OP_16 (0x60). That only ever applied to
        // the first 16 blocks, which no pool is mining anymore, so we don't
        // read them.
        assert_eq!(
            bip34_coinbase_block_height(&ScriptBuf::from_hex("51").unwrap()),
            None
        );
        assert_eq!(
            bip34_coinbase_block_height(&ScriptBuf::from_hex("60").unwrap()),
            None
        );
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

    #[test]
    fn test_decode_hex_odd_length() {
        assert_eq!(decode_hex("abc"), Err(HexDecodeError::OddLength));
        assert_eq!(decode_hex("0"), Err(HexDecodeError::OddLength));
        assert_eq!(decode_hex("0xabc"), Err(HexDecodeError::OddLength));
    }

    #[test]
    fn test_decode_hex_invalid_input() {
        // Not a hex digit.
        assert!(matches!(
            decode_hex("zz"),
            Err(HexDecodeError::InvalidDigit(_))
        ));
        // Multi byte characters must not panic either, no matter where in the
        // string they are and how long they are.
        for input in ["ä", "ää", "aä", "äa", "€", "a€", "€a", "aa€b"] {
            assert!(
                decode_hex(input).is_err(),
                "decoding {} should fail, not panic",
                input
            );
        }
    }

    #[test]
    fn test_decode_hex_error_messages() {
        use std::error::Error;

        let odd = decode_hex("abc").unwrap_err();
        assert_eq!(odd.to_string(), "hex string has an odd number of digits");
        assert!(odd.source().is_none());

        let invalid = decode_hex("zz").unwrap_err();
        let message = invalid.to_string();
        assert!(
            message.starts_with("hex string has an invalid digit: "),
            "unexpected message: {}",
            message
        );
        // The underlying parse error is kept so callers can inspect it.
        let source = invalid
            .source()
            .expect("invalid digit should have a source");
        assert!(message.ends_with(&source.to_string()));
    }

    #[test]
    fn test_decode_hex_empty() {
        assert_eq!(decode_hex(""), Ok(vec![]));
        assert_eq!(decode_hex("0x"), Ok(vec![]));
    }

    #[test]
    fn test_decode_hex_uppercase() {
        assert_eq!(decode_hex("DEADBEEF"), Ok(vec![222, 173, 190, 239]));
        assert_eq!(decode_hex("0xDeAdBeEf"), Ok(vec![222, 173, 190, 239]));
    }

    #[test]
    fn test_encode_decode_hex_roundtrip() {
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            vec![0],
            vec![0xff],
            vec![0x00, 0x01, 0x7f, 0x80, 0xfe, 0xff],
            (0u8..=255).collect(),
        ];
        for bytes in cases {
            let hex = encode_hex(&bytes);
            assert_eq!(hex.len(), bytes.len() * 2);
            assert_eq!(decode_hex(&hex), Ok(bytes));
        }
    }
}
