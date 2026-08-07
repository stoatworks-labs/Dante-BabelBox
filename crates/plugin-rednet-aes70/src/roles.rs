//! Turning a device's own object names into this project's role convention.
//!
//! The host resolves `bridge.toml` mappings by looking for a `settable`
//! descriptor whose role is exactly `"Ch {n} Gain"` or `"Ch {n} Phantom"` (see
//! `docs/plugin-development-guide.md`). AES70 devices don't name their objects
//! that way — they name them however the vendor liked — so something has to
//! bridge the two.
//!
//! **This module is the one guess in this plugin, and it is a guess about
//! naming, not about the wire.** Everything else here either comes off the
//! device at runtime or out of the AES70 standard. No RedNet device's actual
//! role strings have been observed, so the patterns below are the plausible
//! ones, and any object that doesn't match keeps the device's own name
//! verbatim rather than being forced into a preamp shape. If real role strings
//! turn out to differ, this is the only file that should need to change.

use dante_babelbox_oca::OcaClass;

/// The two fields the host's channel-mapping shorthand knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Gain,
    Phantom,
}

impl Field {
    fn suffix(self) -> &'static str {
        match self {
            Field::Gain => "Gain",
            Field::Phantom => "Phantom",
        }
    }
}

/// Build the host-recognised role for a channel and field.
pub fn canonical(channel: u16, field: Field) -> String {
    format!("Ch {channel} {}", field.suffix())
}

/// Work out the canonical role for one discovered object, or `None` if it
/// isn't a per-channel gain or phantom control.
///
/// `leaf` is the object's own role; `path` is the roles of its containing
/// blocks, outermost first. The channel number is looked for in the leaf first
/// and then in the path from the innermost block outwards, because a device
/// may name either the object ("Ch 3 Gain") or its container ("Input 3" ->
/// "Gain").
pub fn classify(leaf: &str, path: &[String], class: OcaClass) -> Option<String> {
    let field = field_of(leaf, class)?;
    let channel = std::iter::once(leaf)
        .chain(path.iter().rev().map(String::as_str))
        .find_map(channel_number)?;
    Some(canonical(channel, field))
}

/// Which field a leaf role names, guarded by the object's OCA class so a
/// *label* that happens to contain "gain" can't be mistaken for a gain control.
fn field_of(leaf: &str, class: OcaClass) -> Option<Field> {
    let lower = leaf.to_ascii_lowercase();

    // "Gain Compensation" on a RedNet MP8R is a unit-wide DSP feature, not a
    // preamp gain, and its per-channel form would otherwise match below.
    if lower.contains("compensat") {
        return None;
    }

    let gainish = matches!(class, OcaClass::Gain);
    let switchish = matches!(class, OcaClass::Switch | OcaClass::Mute);

    if gainish && (lower.contains("gain") || lower.contains("trim")) {
        return Some(Field::Gain);
    }
    if switchish && (lower.contains("phantom") || lower.contains("48v") || lower.contains("+48")) {
        return Some(Field::Phantom);
    }
    None
}

/// Pull a 1-based channel number out of a role string.
///
/// Accepts the labelled forms a device is likely to use ("Ch 3", "Channel 3",
/// "Input 3", "In 3", "Mic 3", "Preamp 3") and, failing those, a trailing
/// number. Requires the number to be plausible as a channel so a role like
/// "HPF 65Hz" or "Gain Compensation -3" can't be read as one.
fn channel_number(text: &str) -> Option<u16> {
    const LABELS: &[&str] = &["channel", "chan", "ch", "input", "in", "mic", "preamp", "pre"];
    let lower = text.to_ascii_lowercase();

    for label in LABELS {
        let mut from = 0;
        while let Some(at) = lower[from..].find(label) {
            let start = from + at;
            let after = start + label.len();
            // The label must be a whole word, not the tail of another one
            // ("in" inside "gain"), and must be followed by the number.
            let boundary_before = start == 0 || !is_word_byte(lower.as_bytes()[start - 1]);
            if boundary_before {
                if let Some(n) = leading_number(lower[after..].trim_start_matches([' ', '.', '-', '_', '#'])) {
                    if is_plausible_channel(n) {
                        return Some(n);
                    }
                }
            }
            from = after;
        }
    }

    // A block named nothing but its own number, e.g. "3". Deliberately the
    // *whole* string and not a trailing number: "Gain 65" ends in a plausible
    // integer without that integer being a channel.
    lower.trim().parse().ok().filter(|n| is_plausible_channel(*n))
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

fn leading_number(text: &str) -> Option<u16> {
    let digits: String = text.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Channel numbers are 1-based and small. 256 is well past any Dante preamp
/// while still rejecting the frequencies and levels that appear in role names.
fn is_plausible_channel(n: u16) -> bool {
    (1..=256).contains(&n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_channel_off_the_leaf_role() {
        assert_eq!(classify("Ch 3 Gain", &[], OcaClass::Gain).as_deref(), Some("Ch 3 Gain"));
        assert_eq!(classify("Input 12 Gain", &[], OcaClass::Gain).as_deref(), Some("Ch 12 Gain"));
    }

    #[test]
    fn reads_a_channel_off_the_containing_block() {
        let path = vec!["Inputs".to_string(), "Input 3".to_string()];
        assert_eq!(classify("Gain", &path, OcaClass::Gain).as_deref(), Some("Ch 3 Gain"));
        assert_eq!(
            classify("Phantom Power", &path, OcaClass::Switch).as_deref(),
            Some("Ch 3 Phantom")
        );
    }

    /// The innermost block wins: an outer "Inputs 1-8" block must not override
    /// the per-channel block inside it.
    #[test]
    fn the_innermost_container_supplies_the_channel() {
        let path = vec!["Mic 1".to_string(), "Channel 7".to_string()];
        assert_eq!(classify("Gain", &path, OcaClass::Gain).as_deref(), Some("Ch 7 Gain"));
    }

    #[test]
    fn phantom_is_recognised_in_its_common_spellings() {
        let path = vec!["Input 2".to_string()];
        for leaf in ["Phantom", "+48V", "48V Enable", "Phantom Power"] {
            assert_eq!(
                classify(leaf, &path, OcaClass::Switch).as_deref(),
                Some("Ch 2 Phantom"),
                "leaf {leaf:?}"
            );
        }
    }

    /// The class is the guard: a string label containing "gain" is not a gain
    /// control, and a gain object is not a phantom switch.
    #[test]
    fn the_oca_class_gates_the_field() {
        let path = vec!["Input 4".to_string()];
        assert_eq!(classify("Gain", &path, OcaClass::StringSensor), None);
        assert_eq!(classify("Phantom", &path, OcaClass::Gain), None);
        assert_eq!(classify("Gain", &path, OcaClass::Gain).as_deref(), Some("Ch 4 Gain"));
    }

    /// An MP8R's gain compensation is a different feature that happens to
    /// contain the word "gain" — mapping it as a preamp gain would let the host
    /// drive the DSP split instead of the mic amp.
    #[test]
    fn gain_compensation_is_not_a_preamp_gain() {
        let path = vec!["Input 5".to_string()];
        assert_eq!(classify("Gain Compensation", &path, OcaClass::Gain), None);
        assert_eq!(classify("Compensated Gain", &path, OcaClass::Gain), None);
    }

    /// "in" appears inside "gain"; matching it as a label would read the "65"
    /// out of "HPF 65Hz" or invent a channel from nowhere.
    #[test]
    fn a_label_must_be_a_whole_word() {
        assert_eq!(channel_number("gain 65"), None);
        assert_eq!(channel_number("Bargain 3"), None);
        assert_eq!(channel_number("In 3"), Some(3));
    }

    /// A block named only its own number is the one unlabelled form accepted —
    /// and it has to be the *whole* string, or "Gain 65" reads as channel 65.
    #[test]
    fn a_bare_number_is_a_channel_only_when_it_is_the_entire_name() {
        assert_eq!(channel_number("3"), Some(3));
        assert_eq!(channel_number(" 12 "), Some(12));
        assert_eq!(channel_number("Gain 65"), None);
        assert_eq!(channel_number("Delay 5"), None);
    }

    #[test]
    fn implausible_numbers_are_not_channels() {
        assert_eq!(channel_number("HPF 65Hz"), None);
        assert_eq!(channel_number("Ch 0"), None);
        assert_eq!(channel_number("Input 9999"), None);
    }

    #[test]
    fn an_unmatched_object_gets_no_canonical_role() {
        assert_eq!(classify("Sample Rate", &[], OcaClass::Int32Sensor), None);
        assert_eq!(classify("Gain", &[], OcaClass::Gain), None); // no channel anywhere
    }

    #[test]
    fn canonical_matches_the_hosts_expected_format() {
        assert_eq!(canonical(3, Field::Gain), "Ch 3 Gain");
        assert_eq!(canonical(16, Field::Phantom), "Ch 16 Phantom");
    }
}
