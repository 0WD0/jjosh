use std::io;
use std::str::FromStr;

use crate::{Error, Time};

/// A container for just enough bytes to hold the largest-possible [`time`](Time) instance.
#[derive(Default, Clone)]
pub struct TimeBuf {
    idx: usize,
    buf: [u8; Time::MAX.size()],
}

struct TimeBufWriter<'a>(&'a mut TimeBuf);

impl io::Write for TimeBufWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, io::Error> {
        let idx = self.0.idx;
        let end_idx = idx
            .checked_add(buf.len())
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        if end_idx > Time::MAX.size() {
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        self.0.buf[idx..end_idx].copy_from_slice(buf);
        self.0.idx = end_idx;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl TimeBuf {
    /// Represent this instance as standard string, serialized in a format compatible with
    /// signature fields in Git commits, also known as anything parseable as [raw format](function::parse_header()).
    pub fn as_str(&self) -> &str {
        // Time serializes as ASCII, which is a subset of UTF-8.
        let time_bytes = &self.buf[..self.idx];
        std::str::from_utf8(time_bytes).expect("time serializes as valid UTF-8")
    }

    /// Clear the previous content.
    fn clear(&mut self) {
        self.idx = 0;
    }
}

impl Time {
    /// Serialize this instance into `buf`, exactly as it would appear in the header of a Git commit,
    /// and return `buf` as `&str` for easy consumption.
    pub fn to_str<'a>(&self, buf: &'a mut TimeBuf) -> &'a str {
        buf.clear();
        self.write_to(&mut TimeBufWriter(buf))
            .expect("write to memory of just the right size cannot fail");
        buf.as_str()
    }
}

impl FromStr for Time {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        crate::parse_header(s).ok_or_else(|| Error::new_with_input("invalid time", s))
    }
}

pub(crate) mod function;
mod git;
mod raw;
mod relative;
