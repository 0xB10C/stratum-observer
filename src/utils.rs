use std::convert::TryFrom;
use std::fmt::Write;
use sv1_api::utils::{MerkleNode, PrevHash};

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

fn merklenode_from_hex<'a>(hex: &str) -> MerkleNode<'a> {
    let data = decode_hex(hex).unwrap();
    let len = data.len();
    if hex.len() >= 64 {
        // panic if hex is larger than 32 bytes
        MerkleNode::try_from(hex).expect("Failed to convert hex to U256")
    } else {
        // prepend hex with zeros so that it is 32 bytes
        let mut new_vec = vec![0_u8; 32 - len];
        new_vec.extend(data.iter());
        MerkleNode::try_from(encode_hex(&new_vec).as_str()).expect("Failed to convert hex to U256")
    }
}

fn prevhash_from_hex<'a>(hex: &str) -> PrevHash<'a> {
    let data = decode_hex(hex).unwrap();
    let len = data.len();
    if hex.len() >= 64 {
        // panic if hex is larger than 32 bytes
        PrevHash::try_from(hex).expect("Failed to convert hex to U256")
    } else {
        // prepend hex with zeros so that it is 32 bytes
        let mut new_vec = vec![0_u8; 32 - len];
        new_vec.extend(data.iter());
        PrevHash::try_from(encode_hex(&new_vec).as_str()).expect("Failed to convert hex to U256")
    }
}

#[cfg(test)]
#[test]
fn test() {
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

    // reencode
    let reencoded = encode_hex(&decoded);
    let reencoded_prefix = encode_hex(&decoded);

    assert_eq!(&reencoded, hex_og, "Hex not encoded correctly");
    assert_eq!(
        &reencoded_prefix, &hex_og,
        "Hex w/ prefix not encoded correctly"
    );
}
