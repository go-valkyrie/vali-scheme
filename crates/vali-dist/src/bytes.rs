//! The encoding's primitives: LEB128 integers, lengths, strings and bytes.
//!
//! A [`Reader`] never panics and never allocates more than the input could
//! hold: every count is checked against the bytes left before anything is
//! reserved for it, so a corrupt or hostile artifact fails with a
//! [`DecodeError`].

use std::fmt;

/// Why bytes could not be decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError {
    /// How far into the input the problem was found.
    pub offset: usize,
    pub message: String,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "at byte {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for DecodeError {}

/// Appends encoded values to a buffer.
#[derive(Debug, Default)]
pub struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    pub fn new() -> Writer {
        Writer::default()
    }

    /// The bytes written.
    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn byte(&mut self, b: u8) {
        self.bytes.push(b);
    }

    pub fn bool(&mut self, b: bool) {
        self.byte(u8::from(b));
    }

    /// An unsigned integer, LEB128: seven bits a byte, low first, the top
    /// bit set on every byte but the last.
    pub fn uint(&mut self, mut n: u64) {
        loop {
            let low = (n & 0x7F) as u8;
            n >>= 7;
            if n == 0 {
                self.byte(low);
                return;
            }
            self.byte(low | 0x80);
        }
    }

    /// A signed integer, zigzag then LEB128, so small magnitudes are short
    /// either side of zero.
    pub fn int(&mut self, n: i64) {
        self.uint(((n << 1) ^ (n >> 63)) as u64);
    }

    pub fn usize(&mut self, n: usize) {
        self.uint(n as u64);
    }

    pub fn u32(&mut self, n: u32) {
        self.uint(u64::from(n));
    }

    /// A flonum, its IEEE 754 bits, little-endian.
    pub fn f64(&mut self, x: f64) {
        self.bytes.extend_from_slice(&x.to_bits().to_le_bytes());
    }

    /// Bytes, after their length.
    pub fn bytes(&mut self, bytes: &[u8]) {
        self.usize(bytes.len());
        self.bytes.extend_from_slice(bytes);
    }

    /// A string, as its UTF-8 bytes.
    pub fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }

    /// Raw bytes, with no length: the caller knows where they end.
    pub fn raw(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }
}

/// Reads encoded values from a buffer.
#[derive(Clone, Debug)]
pub struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Reader<'a> {
        Reader { bytes, at: 0 }
    }

    /// An error at the current position.
    pub fn error(&self, message: impl Into<String>) -> DecodeError {
        DecodeError {
            offset: self.at,
            message: message.into(),
        }
    }

    /// How many bytes have been read.
    pub fn offset(&self) -> usize {
        self.at
    }

    /// How many bytes are left.
    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Fails unless every byte was read.
    pub fn end(&self) -> Result<(), DecodeError> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(self.error(format!("{} bytes left over", self.remaining())))
        }
    }

    pub fn byte(&mut self) -> Result<u8, DecodeError> {
        let b = *self
            .bytes
            .get(self.at)
            .ok_or_else(|| self.error("unexpected end of input"))?;
        self.at += 1;
        Ok(b)
    }

    pub fn bool(&mut self) -> Result<bool, DecodeError> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            b => Err(self.error(format!("{b} is not a boolean"))),
        }
    }

    pub fn uint(&mut self) -> Result<u64, DecodeError> {
        let mut n = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            let bits = u64::from(b & 0x7F);
            if shift == 63 && bits > 1 {
                return Err(self.error("an integer overflows 64 bits"));
            }
            n |= bits << shift;
            if b & 0x80 == 0 {
                return Ok(n);
            }
        }
        Err(self.error("an integer overflows 64 bits"))
    }

    pub fn int(&mut self) -> Result<i64, DecodeError> {
        let n = self.uint()?;
        Ok(((n >> 1) as i64) ^ -((n & 1) as i64))
    }

    pub fn u32(&mut self) -> Result<u32, DecodeError> {
        let n = self.uint()?;
        u32::try_from(n).map_err(|_| self.error(format!("{n} does not fit in 32 bits")))
    }

    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        let n = self.uint()?;
        u8::try_from(n).map_err(|_| self.error(format!("{n} does not fit in 8 bits")))
    }

    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        let n = self.uint()?;
        u16::try_from(n).map_err(|_| self.error(format!("{n} does not fit in 16 bits")))
    }

    /// A count of things each at least `min_bytes` long: no more than the
    /// bytes left could hold, so nothing is reserved for more.
    pub fn count(&mut self, min_bytes: usize) -> Result<usize, DecodeError> {
        let n = self.uint()?;
        let most = self.remaining() / min_bytes.max(1);
        match usize::try_from(n) {
            Ok(n) if n <= most => Ok(n),
            _ => Err(self.error(format!("a count of {n} exceeds what is left"))),
        }
    }

    pub fn f64(&mut self) -> Result<f64, DecodeError> {
        let bytes = self.raw(8)?;
        let mut word = [0; 8];
        word.copy_from_slice(bytes);
        Ok(f64::from_bits(u64::from_le_bytes(word)))
    }

    /// `n` raw bytes.
    pub fn raw(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if n > self.remaining() {
            return Err(self.error("unexpected end of input"));
        }
        let bytes = &self.bytes[self.at..self.at + n];
        self.at += n;
        Ok(bytes)
    }

    /// Bytes, after their length.
    pub fn bytes(&mut self) -> Result<&'a [u8], DecodeError> {
        let n = self.count(1)?;
        self.raw(n)
    }

    pub fn str(&mut self) -> Result<&'a str, DecodeError> {
        let at = self.at;
        let bytes = self.bytes()?;
        std::str::from_utf8(bytes).map_err(|_| DecodeError {
            offset: at,
            message: "a string is not UTF-8".into(),
        })
    }
}
