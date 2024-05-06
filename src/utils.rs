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
}
