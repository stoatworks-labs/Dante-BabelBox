//! Marshalling of the AES70-1 base data types onto the AES70-3 (OCP.1) wire.
//!
//! Everything is big-endian. Variable-length types (`OcaString`, `OcaBlob`,
//! `OcaList`) are length-prefixed with a `u16` count.

use crate::Error;

/// Cursor over a received parameter blob.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

macro_rules! read_int {
    ($name:ident, $ty:ty, $n:expr) => {
        pub fn $name(&mut self) -> Result<$ty, Error> {
            let b = self.take($n)?;
            Ok(<$ty>::from_be_bytes(b.try_into().expect("slice length checked by take")))
        }
    };
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn remaining(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(n).ok_or(Error::Truncated)?;
        let slice = self.buf.get(self.pos..end).ok_or(Error::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    read_int!(u8, u8, 1);
    read_int!(u16, u16, 2);
    read_int!(u32, u32, 4);
    read_int!(u64, u64, 8);
    read_int!(i8, i8, 1);
    read_int!(i16, i16, 2);
    read_int!(i32, i32, 4);
    read_int!(i64, i64, 8);
    read_int!(f32, f32, 4);
    read_int!(f64, f64, 8);

    pub fn bool(&mut self) -> Result<bool, Error> {
        Ok(self.u8()? != 0)
    }

    /// `OcaBlob` / `OcaString` body: `u16` length followed by that many octets.
    pub fn bytes(&mut self) -> Result<&'a [u8], Error> {
        let len = self.u16()? as usize;
        self.take(len)
    }

    pub fn string(&mut self) -> Result<String, Error> {
        // Devices are not always strict about UTF-8 in role names; don't fail the
        // whole enumeration over one bad byte.
        Ok(String::from_utf8_lossy(self.bytes()?).into_owned())
    }

    /// `OcaList<T>`: `u16` count followed by that many elements.
    pub fn list<T, F>(&mut self, mut item: F) -> Result<Vec<T>, Error>
    where
        F: FnMut(&mut Self) -> Result<T, Error>,
    {
        let count = self.u16()? as usize;
        let mut out = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            out.push(item(self)?);
        }
        Ok(out)
    }
}

/// Accumulator for outgoing parameters.
#[derive(Debug, Default, Clone)]
pub struct Writer {
    buf: Vec<u8>,
}

macro_rules! write_int {
    ($name:ident, $ty:ty) => {
        pub fn $name(&mut self, v: $ty) -> &mut Self {
            self.buf.extend_from_slice(&v.to_be_bytes());
            self
        }
    };
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    write_int!(u8, u8);
    write_int!(u16, u16);
    write_int!(u32, u32);
    write_int!(u64, u64);
    write_int!(i8, i8);
    write_int!(i16, i16);
    write_int!(i32, i32);
    write_int!(i64, i64);
    write_int!(f32, f32);
    write_int!(f64, f64);

    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.u8(u8::from(v))
    }

    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.u16(v.len() as u16);
        self.buf.extend_from_slice(v);
        self
    }

    pub fn string(&mut self, v: &str) -> &mut Self {
        self.bytes(v.as_bytes())
    }

    pub fn raw(&mut self, v: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_scalars_big_endian() {
        let mut w = Writer::new();
        w.u32(0x0102_0304).f32(-57.5).bool(true).string("Gain");
        let bytes = w.finish();
        assert_eq!(&bytes[..4], &[0x01, 0x02, 0x03, 0x04]);

        let mut r = Reader::new(&bytes);
        assert_eq!(r.u32().unwrap(), 0x0102_0304);
        assert_eq!(r.f32().unwrap(), -57.5);
        assert!(r.bool().unwrap());
        assert_eq!(r.string().unwrap(), "Gain");
        assert!(r.is_empty());
    }

    #[test]
    fn reader_reports_truncation_instead_of_panicking() {
        let mut r = Reader::new(&[0x00, 0x04, 0xff]);
        assert!(matches!(r.bytes(), Err(Error::Truncated)));
    }

    #[test]
    fn list_is_u16_count_prefixed() {
        let mut w = Writer::new();
        w.u16(3).u32(10).u32(20).u32(30);
        let bytes = w.finish();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.list(|r| r.u32()).unwrap(), vec![10, 20, 30]);
    }
}
