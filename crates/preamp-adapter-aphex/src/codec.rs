//! The 1788A parametric SysEx string: build one, parse one.
//!
//! ```text
//! F0 | 00 00 38 | MIDI Channel | MIDI Device | Net Number
//!    | [Command, Mic Channel, Value] x 1..=64
//!    | F7
//! ```

use crate::Error;

pub const SYSEX_START: u8 = 0xF0;
pub const SYSEX_END: u8 = 0xF7;
/// Aphex's three-byte MIDI manufacturer id. Three-byte ids are the extended
/// form and always begin `00`, which is why this is `00 00 38` and not `38`.
pub const APHEX_MANUFACTURER_ID: [u8; 3] = [0x00, 0x00, 0x38];

/// The largest value any byte inside a SysEx message may take. Everything from
/// `F0` to `F7` is a MIDI *data* byte, so bit 7 must be clear — a value of
/// `0x80` or above would be read as the start of a new MIDI message and corrupt
/// the stream. This is the invariant every constructor in this module enforces.
pub const MAX_DATA_BYTE: u8 = 0x7F;

/// The documented ceiling on commands per message.
pub const MAX_COMMANDS: usize = 64;

/// Bytes before the command list: `F0`, three id bytes, and the three address
/// bytes.
const HEADER_LEN: usize = 7;

/// Which control a command addresses.
///
/// Only the opcodes on the published command table are represented. The table
/// is not contiguous — `09h`, and everything between `0Ch`–`16h` and
/// `18h`–`1Fh`, are absent from it — so those are rejected rather than assumed
/// to be unused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    InputGain,
    MainMaxOut,
    AuxMaxOut,
    PhantomPower,
    LowCutFilter,
    /// "MicLim" on the front panel.
    Limiter,
    PolarityReverse,
    Mute,
    Pad,
    TestToneMinus20dB,
    TestTone0dB,
    /// The table's "alternative method": one control with three states rather
    /// than two separate on/off switches.
    TestTone,
    RequestParameterDump,
    RequestExtendedDump,
}

impl Control {
    pub const fn opcode(self) -> u8 {
        match self {
            Control::InputGain => 0x00,
            Control::MainMaxOut => 0x01,
            Control::AuxMaxOut => 0x02,
            Control::PhantomPower => 0x03,
            Control::LowCutFilter => 0x04,
            Control::Limiter => 0x05,
            Control::PolarityReverse => 0x06,
            Control::Mute => 0x07,
            Control::Pad => 0x08,
            Control::TestToneMinus20dB => 0x0A,
            Control::TestTone0dB => 0x0B,
            Control::TestTone => 0x17,
            Control::RequestParameterDump => 0x20,
            Control::RequestExtendedDump => 0x56,
        }
    }

    pub const fn from_opcode(opcode: u8) -> Option<Self> {
        Some(match opcode {
            0x00 => Control::InputGain,
            0x01 => Control::MainMaxOut,
            0x02 => Control::AuxMaxOut,
            0x03 => Control::PhantomPower,
            0x04 => Control::LowCutFilter,
            0x05 => Control::Limiter,
            0x06 => Control::PolarityReverse,
            0x07 => Control::Mute,
            0x08 => Control::Pad,
            0x0A => Control::TestToneMinus20dB,
            0x0B => Control::TestTone0dB,
            0x17 => Control::TestTone,
            0x20 => Control::RequestParameterDump,
            0x56 => Control::RequestExtendedDump,
            _ => return None,
        })
    }

    /// The inclusive value range the command table gives for this control.
    pub const fn value_range(self) -> (u8, u8) {
        match self {
            // 1Ah to 41h — a direct dB figure, 26 to 65, no scaling.
            Control::InputGain => (0x1A, 0x41),
            Control::MainMaxOut | Control::AuxMaxOut => (0x00, 0x1B),
            Control::TestTone => (0x00, 0x02),
            // "01h (required)" — the only accepted value.
            Control::RequestParameterDump => (0x01, 0x01),
            // "00h (any value placeholder)": the table says any value is
            // tolerated, so nothing narrower than the MIDI limit is enforced.
            Control::RequestExtendedDump => (0x00, MAX_DATA_BYTE),
            _ => (0x00, 0x01),
        }
    }

    /// Whether this control is a plain on/off switch.
    pub const fn is_switch(self) -> bool {
        matches!(
            self,
            Control::PhantomPower
                | Control::LowCutFilter
                | Control::Limiter
                | Control::PolarityReverse
                | Control::Mute
                | Control::Pad
                | Control::TestToneMinus20dB
                | Control::TestTone0dB
        )
    }
}

/// Which unit on the link a message is for.
///
/// All three bytes come straight from the message layout. What each one selects
/// — and how it relates to the unit's front-panel settings — isn't on the
/// command-table page, so nothing here interprets them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceAddress {
    pub midi_channel: u8,
    pub midi_device: u8,
    pub net_number: u8,
}

impl DeviceAddress {
    pub fn new(midi_channel: u8, midi_device: u8, net_number: u8) -> Result<Self, Error> {
        for (name, value) in
            [("midi_channel", midi_channel), ("midi_device", midi_device), ("net_number", net_number)]
        {
            if value > MAX_DATA_BYTE {
                return Err(Error::NotADataByte { field: name, value });
            }
        }
        Ok(Self { midi_channel, midi_device, net_number })
    }
}

/// One 3-byte parametric command.
///
/// `mic_channel` is the raw byte the command table labels "Mic Channel". The
/// page does not say whether inputs are numbered from 0 or from 1, nor whether
/// any value means "all channels", so this type carries the byte as-is and
/// leaves that mapping to a caller that has hardware to check it against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    pub control: Control,
    pub mic_channel: u8,
    pub value: u8,
}

impl Command {
    /// Build a command, rejecting anything the command table doesn't allow.
    pub fn new(control: Control, mic_channel: u8, value: u8) -> Result<Self, Error> {
        if mic_channel > MAX_DATA_BYTE {
            return Err(Error::NotADataByte { field: "mic_channel", value: mic_channel });
        }
        let (min, max) = control.value_range();
        if !(min..=max).contains(&value) {
            return Err(Error::ValueOutOfRange { control, value, min, max });
        }
        Ok(Self { control, mic_channel, value })
    }

    /// Input gain in whole dB. The 1788A's range is 26–65 dB and the wire value
    /// is that number directly.
    pub fn input_gain_db(mic_channel: u8, gain_db: u8) -> Result<Self, Error> {
        Self::new(Control::InputGain, mic_channel, gain_db)
    }

    /// Any of the on/off controls.
    pub fn switch(control: Control, mic_channel: u8, on: bool) -> Result<Self, Error> {
        if !control.is_switch() {
            return Err(Error::NotASwitch(control));
        }
        Self::new(control, mic_channel, u8::from(on))
    }

    fn to_bytes(self) -> [u8; 3] {
        [self.control.opcode(), self.mic_channel, self.value]
    }
}

/// A complete parametric control string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub address: DeviceAddress,
    pub commands: Vec<Command>,
}

impl Message {
    pub fn new(address: DeviceAddress, commands: Vec<Command>) -> Result<Self, Error> {
        if commands.is_empty() {
            return Err(Error::NoCommands);
        }
        if commands.len() > MAX_COMMANDS {
            return Err(Error::TooManyCommands(commands.len()));
        }
        Ok(Self { address, commands })
    }

    /// Encode to the wire.
    ///
    /// Every byte this emits between `F0` and `F7` is guaranteed to be a legal
    /// data byte, because the only ways to build a [`Command`] or a
    /// [`DeviceAddress`] check that up front.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.commands.len() * 3 + 1);
        out.push(SYSEX_START);
        out.extend_from_slice(&APHEX_MANUFACTURER_ID);
        out.push(self.address.midi_channel);
        out.push(self.address.midi_device);
        out.push(self.address.net_number);
        for command in &self.commands {
            out.extend_from_slice(&command.to_bytes());
        }
        out.push(SYSEX_END);
        out
    }

    /// Decode a complete SysEx frame.
    ///
    /// **This also assumes the unit's replies to `20h`/`56h` come back in this
    /// same shape.** That is the natural reading — a dump is only useful if it
    /// is parseable, and the command table describes no other format — but the
    /// page documents the *control* string, not the reply, so treat a
    /// successful parse of a dump as unconfirmed until it is seen on the wire.
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < HEADER_LEN + 3 + 1 {
            return Err(Error::Truncated);
        }
        if bytes[0] != SYSEX_START {
            return Err(Error::NotSysEx(bytes[0]));
        }
        if bytes[bytes.len() - 1] != SYSEX_END {
            return Err(Error::Unterminated);
        }
        if bytes[1..4] != APHEX_MANUFACTURER_ID {
            return Err(Error::ForeignManufacturer([bytes[1], bytes[2], bytes[3]]));
        }

        let body = &bytes[HEADER_LEN..bytes.len() - 1];
        // A stray data byte would silently shift every field after it, so a
        // body that isn't a whole number of commands is rejected outright.
        if !body.len().is_multiple_of(3) {
            return Err(Error::RaggedBody(body.len()));
        }

        let address = DeviceAddress::new(bytes[4], bytes[5], bytes[6])?;
        let commands = body
            .chunks_exact(3)
            .map(|c| {
                let control = Control::from_opcode(c[0]).ok_or(Error::UnknownOpcode(c[0]))?;
                Command::new(control, c[1], c[2])
            })
            .collect::<Result<Vec<_>, _>>()?;

        Message::new(address, commands)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address() -> DeviceAddress {
        DeviceAddress::new(0, 0, 1).unwrap()
    }

    /// The frame layout straight off the published page.
    #[test]
    fn encodes_the_documented_frame_layout() {
        let message = Message::new(
            address(),
            vec![Command::input_gain_db(1, 40).unwrap()],
        )
        .unwrap();

        assert_eq!(
            message.encode(),
            vec![0xF0, 0x00, 0x00, 0x38, 0x00, 0x00, 0x01, 0x00, 0x01, 40, 0xF7]
        );
    }

    #[test]
    fn gain_is_a_direct_db_figure_not_a_scaled_one() {
        // 26 dB is the bottom of the range and encodes as 0x1A, 65 as 0x41 -
        // exactly the numbers the table gives.
        assert_eq!(Command::input_gain_db(1, 26).unwrap().value, 0x1A);
        assert_eq!(Command::input_gain_db(1, 65).unwrap().value, 0x41);
    }

    #[test]
    fn gain_outside_the_documented_range_is_refused() {
        assert!(matches!(Command::input_gain_db(1, 25), Err(Error::ValueOutOfRange { .. })));
        assert!(matches!(Command::input_gain_db(1, 66), Err(Error::ValueOutOfRange { .. })));
    }

    /// The invariant that matters most: nothing inside a SysEx frame may have
    /// bit 7 set, or a receiver reads it as the start of a new MIDI message.
    #[test]
    fn no_encoded_byte_between_the_delimiters_has_bit_7_set() {
        let commands = vec![
            Command::input_gain_db(1, 65).unwrap(),
            Command::switch(Control::PhantomPower, 8, true).unwrap(),
            Command::new(Control::RequestExtendedDump, MAX_DATA_BYTE, MAX_DATA_BYTE).unwrap(),
        ];
        let bytes = Message::new(
            DeviceAddress::new(MAX_DATA_BYTE, MAX_DATA_BYTE, MAX_DATA_BYTE).unwrap(),
            commands,
        )
        .unwrap()
        .encode();

        assert_eq!(bytes[0], SYSEX_START);
        assert_eq!(*bytes.last().unwrap(), SYSEX_END);
        assert!(
            bytes[1..bytes.len() - 1].iter().all(|b| *b <= MAX_DATA_BYTE),
            "a status byte leaked into the payload: {bytes:02x?}"
        );
    }

    #[test]
    fn an_out_of_range_data_byte_is_refused_rather_than_truncated() {
        assert!(matches!(
            Command::new(Control::RequestExtendedDump, 0x80, 0),
            Err(Error::NotADataByte { field: "mic_channel", .. })
        ));
        assert!(matches!(
            DeviceAddress::new(0, 0x80, 0),
            Err(Error::NotADataByte { field: "midi_device", .. })
        ));
    }

    #[test]
    fn switches_take_only_off_and_on() {
        let on = Command::switch(Control::Pad, 3, true).unwrap();
        assert_eq!(on.value, 0x01);
        assert_eq!(Command::switch(Control::Pad, 3, false).unwrap().value, 0x00);
        assert!(matches!(Command::new(Control::Pad, 3, 2), Err(Error::ValueOutOfRange { .. })));
    }

    /// The three-state test tone is not a switch, so the switch helper must not
    /// silently collapse it to on/off.
    #[test]
    fn the_three_state_test_tone_is_not_a_switch() {
        assert!(matches!(Command::switch(Control::TestTone, 1, true), Err(Error::NotASwitch(_))));
        assert_eq!(Command::new(Control::TestTone, 1, 2).unwrap().value, 2);
        assert!(matches!(Command::new(Control::TestTone, 1, 3), Err(Error::ValueOutOfRange { .. })));
    }

    #[test]
    fn a_parameter_dump_request_takes_only_the_required_value() {
        assert!(Command::new(Control::RequestParameterDump, 0, 0x01).is_ok());
        assert!(matches!(
            Command::new(Control::RequestParameterDump, 0, 0x00),
            Err(Error::ValueOutOfRange { .. })
        ));
    }

    #[test]
    fn round_trips_a_multi_command_message() {
        let message = Message::new(
            DeviceAddress::new(2, 3, 4).unwrap(),
            vec![
                Command::input_gain_db(1, 40).unwrap(),
                Command::switch(Control::PhantomPower, 1, true).unwrap(),
                Command::switch(Control::Pad, 2, false).unwrap(),
                Command::new(Control::TestTone, 0, 1).unwrap(),
            ],
        )
        .unwrap();

        assert_eq!(Message::decode(&message.encode()).unwrap(), message);
    }

    #[test]
    fn the_documented_batch_ceiling_is_enforced() {
        let one = Command::switch(Control::Mute, 1, true).unwrap();
        assert!(Message::new(address(), vec![one; MAX_COMMANDS]).is_ok());
        assert!(matches!(
            Message::new(address(), vec![one; MAX_COMMANDS + 1]),
            Err(Error::TooManyCommands(65))
        ));
        assert!(matches!(Message::new(address(), vec![]), Err(Error::NoCommands)));
    }

    #[test]
    fn another_manufacturers_sysex_is_not_parsed_as_ours() {
        // Focusrite/Novation's id, from the RedNet MIDI guide.
        let bytes = [0xF0, 0x00, 0x20, 0x29, 0x00, 0x00, 0x00, 0x00, 0x01, 0x28, 0xF7];
        assert!(matches!(Message::decode(&bytes), Err(Error::ForeignManufacturer(_))));
    }

    /// A body that isn't a whole number of 3-byte commands means a byte has
    /// been lost or added, which would shift every field after it.
    #[test]
    fn a_ragged_body_is_rejected_rather_than_silently_realigned() {
        let mut bytes = Message::new(address(), vec![Command::input_gain_db(1, 40).unwrap()])
            .unwrap()
            .encode();
        bytes.insert(8, 0x00);
        assert!(matches!(Message::decode(&bytes), Err(Error::RaggedBody(4))));
    }

    #[test]
    fn malformed_frames_are_rejected() {
        assert!(matches!(Message::decode(&[]), Err(Error::Truncated)));
        assert!(matches!(
            Message::decode(&[0xF7, 0x00, 0x00, 0x38, 0, 0, 0, 0, 0, 0, 0xF7]),
            Err(Error::NotSysEx(0xF7))
        ));
        assert!(matches!(
            Message::decode(&[0xF0, 0x00, 0x00, 0x38, 0, 0, 0, 0, 0, 0, 0x00]),
            Err(Error::Unterminated)
        ));
    }

    /// The command table is not contiguous. An opcode it doesn't list is an
    /// unknown, not an assumed no-op.
    #[test]
    fn an_undocumented_opcode_is_rejected() {
        assert_eq!(Control::from_opcode(0x09), None);
        assert_eq!(Control::from_opcode(0x10), None);
        let bytes = [0xF0, 0x00, 0x00, 0x38, 0, 0, 0, 0x09, 0x01, 0x00, 0xF7];
        assert!(matches!(Message::decode(&bytes), Err(Error::UnknownOpcode(0x09))));
    }

    #[test]
    fn every_documented_opcode_round_trips_through_its_enum() {
        let all = [
            Control::InputGain,
            Control::MainMaxOut,
            Control::AuxMaxOut,
            Control::PhantomPower,
            Control::LowCutFilter,
            Control::Limiter,
            Control::PolarityReverse,
            Control::Mute,
            Control::Pad,
            Control::TestToneMinus20dB,
            Control::TestTone0dB,
            Control::TestTone,
            Control::RequestParameterDump,
            Control::RequestExtendedDump,
        ];
        for control in all {
            assert_eq!(Control::from_opcode(control.opcode()), Some(control));
            let (min, max) = control.value_range();
            assert!(max <= MAX_DATA_BYTE, "{control:?} allows a non-data byte");
            assert!(min <= max);
        }
    }
}
