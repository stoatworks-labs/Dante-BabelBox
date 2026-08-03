//! SLPv2 discovery for ACN-speaking Shure receivers.
//!
//! Every ~2 s each device multicasts an attribute reply. Both the group
//! and the port are **one digit off the registered SLP values**
//! (`239.255.255.253:427`), so a stock SLP library pointed at the standard
//! group finds nothing at all.
//!
//! This is passive: listening enumerates model, name and address for every
//! ACN-speaking receiver on the segment without opening a session or
//! transmitting anything.

/// Not the registered SLP group - note the `254`.
pub const SLP_GROUP: [u8; 4] = [239, 255, 254, 253];
/// Not the registered SLP port either.
pub const SLP_PORT: u16 = 8427;

const SLP_VERSION: u8 = 2;
const FUNCTION_ATTR_RPLY: u8 = 7;
const HEADER_MIN: usize = 14;

/// What a receiver advertises about itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Advertisement {
    /// Component ID. Its first group's low bytes are the device MAC, so
    /// CID and MAC are derivable from each other.
    pub cid: Option<String>,
    /// Fixed model name, e.g. `QLXD4`. A console advertises
    /// `Yamaha Console`.
    pub model: Option<String>,
    /// User-assigned name; defaults to the model name.
    pub user_name: Option<String>,
    /// `host:port` to open an SDT session against.
    pub sdt_endpoint: Option<String>,
    /// Device class ID from the `csl-esta.dmp` attribute.
    pub dcid: Option<String>,
}

impl Advertisement {
    /// True when this looks like a Shure receiver rather than a console or
    /// some other ACN device.
    pub fn is_receiver(&self) -> bool {
        self.model
            .as_deref()
            .is_some_and(|m| m.starts_with("QLXD") || m.starts_with("ULXD") || m.starts_with("AD"))
    }
}

/// Parses an SLPv2 attribute reply.
///
/// Returns `None` for anything that is not a well-formed AttrRply - other
/// SLP functions, other versions, or a truncated datagram.
pub fn parse_attr_reply(datagram: &[u8]) -> Option<Advertisement> {
    if datagram.len() < HEADER_MIN
        || datagram[0] != SLP_VERSION
        || datagram[1] != FUNCTION_ATTR_RPLY
    {
        return None;
    }
    let lang_len = u16::from_be_bytes([datagram[12], datagram[13]]) as usize;
    let body = datagram.get(HEADER_MIN + lang_len..)?;
    // AttrRply body: error code, then a length-prefixed attribute list.
    if body.len() < 4 {
        return None;
    }
    let error = u16::from_be_bytes([body[0], body[1]]);
    if error != 0 {
        return None;
    }
    let attr_len = u16::from_be_bytes([body[2], body[3]]) as usize;
    let attrs = body.get(4..4 + attr_len)?;
    Some(parse_attribute_list(&String::from_utf8_lossy(attrs)))
}

/// Splits `(key=value),(key=value)` into the fields worth having.
///
/// Values can themselves contain commas and semicolons (`csl-esta.dmp`
/// does), so this splits on the parenthesised groups rather than on commas.
pub fn parse_attribute_list(list: &str) -> Advertisement {
    let mut out = Advertisement::default();
    for group in list.split("),(") {
        let group = group.trim_matches(|c| c == '(' || c == ')' || c == ',');
        let Some((key, value)) = group.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "cid" => out.cid = Some(value.to_string()),
            "acn-fctn" => out.model = Some(value.to_string()),
            "acn-uacn" => out.user_name = Some(value.to_string()),
            "csl-esta.dmp" => {
                for part in value.split(';') {
                    if let Some(endpoint) = part.trim().strip_prefix("esta.sdt/") {
                        out.sdt_endpoint = Some(endpoint.to_string());
                    } else if let Some(dcid) = part.trim().strip_prefix("esta.dmp/cd:") {
                        out.dcid = Some(dcid.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The QLXD4's advertisement exactly as recorded in the spec.
    const REAL_ATTRS: &str = "(cid=DD47E0D7-0000-11DD-A000-000EDDCCCCCC),\
(acn-fctn=QLXD4),(acn-uacn=QLXD4),(acn-services=esta.dmp),\
(csl-esta.dmp=esta.sdt/169.254.216.224:57383;esta.dmp/cd:CCEAC054-E139-11DF-84BA-0015C5F3F612),\
(device-description=$:tftp://169.254.216.224/$.ddl)";

    #[test]
    fn parses_the_receivers_real_advertisement() {
        let ad = parse_attribute_list(REAL_ATTRS);
        assert_eq!(ad.model.as_deref(), Some("QLXD4"));
        assert_eq!(ad.user_name.as_deref(), Some("QLXD4"));
        assert_eq!(
            ad.cid.as_deref(),
            Some("DD47E0D7-0000-11DD-A000-000EDDCCCCCC")
        );
        assert_eq!(ad.sdt_endpoint.as_deref(), Some("169.254.216.224:57383"));
        assert_eq!(
            ad.dcid.as_deref(),
            Some("CCEAC054-E139-11DF-84BA-0015C5F3F612")
        );
        assert!(ad.is_receiver());
    }

    #[test]
    fn a_console_advertisement_is_not_mistaken_for_a_receiver() {
        let ad =
            parse_attribute_list("(cid=X),(acn-fctn=Yamaha Console),(acn-uacn=Yamaha Console)");
        assert_eq!(ad.model.as_deref(), Some("Yamaha Console"));
        assert!(!ad.is_receiver());
    }

    #[test]
    fn the_cid_low_bytes_match_the_device_mac() {
        // 00:0e:dd:47:e0:d7 -> DD47E0D7. Documented as derivable, and
        // asserted here so a future change to CID handling notices.
        let ad = parse_attribute_list(REAL_ATTRS);
        assert!(ad.cid.unwrap().starts_with("DD47E0D7"));
    }

    #[test]
    fn parses_a_whole_slp_datagram() {
        let attrs = REAL_ATTRS.as_bytes();
        let mut pkt = vec![
            SLP_VERSION,
            FUNCTION_ATTR_RPLY,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            1,
        ];
        pkt.extend_from_slice(&2u16.to_be_bytes()); // lang tag length
        pkt.extend_from_slice(b"en");
        pkt.extend_from_slice(&0u16.to_be_bytes()); // error code
        pkt.extend_from_slice(&(attrs.len() as u16).to_be_bytes());
        pkt.extend_from_slice(attrs);

        let ad = parse_attr_reply(&pkt).unwrap();
        assert_eq!(ad.model.as_deref(), Some("QLXD4"));
    }

    #[test]
    fn other_slp_functions_and_error_replies_are_ignored() {
        let mut pkt = vec![SLP_VERSION, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        pkt.extend_from_slice(&0u16.to_be_bytes());
        pkt.extend_from_slice(&0u16.to_be_bytes());
        pkt.extend_from_slice(&0u16.to_be_bytes());
        assert!(
            parse_attr_reply(&pkt).is_none(),
            "function 1 is not an AttrRply"
        );

        let mut err = vec![
            SLP_VERSION,
            FUNCTION_ATTR_RPLY,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            1,
        ];
        err.extend_from_slice(&0u16.to_be_bytes());
        err.extend_from_slice(&5u16.to_be_bytes()); // non-zero error
        err.extend_from_slice(&0u16.to_be_bytes());
        assert!(parse_attr_reply(&err).is_none());
    }
}
