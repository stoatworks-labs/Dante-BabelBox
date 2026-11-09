//! Hex literals. The captured reference bytes in this crate are written as
//! hex strings, the way they come out of a packet capture.

/// Decodes a hex string, ignoring whitespace. Panics on anything else: it is
/// only ever called on literals in this crate, and a bad literal is a bug to
/// find in a test, not an error to handle at run time.
pub(crate) fn hex(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    assert!(digits.len().is_multiple_of(2), "odd number of hex digits");
    digits
        .chunks(2)
        .map(|pair| {
            let s = std::str::from_utf8(pair).expect("ascii");
            u8::from_str_radix(s, 16).unwrap_or_else(|_| panic!("'{s}' is not hex"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::hex;

    #[test]
    fn decodes_and_skips_whitespace() {
        assert_eq!(hex("ff 00\n  1d"), vec![0xff, 0x00, 0x1d]);
    }
}
