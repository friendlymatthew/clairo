use anyhow::{Result, ensure};

use utils::{impl_read_for_datatype_le, impl_read_slice};

use crate::deflate;

pub struct GZipReader<'a> {
    cursor: usize,
    data: &'a [u8],
}

impl<'a> GZipReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { cursor: 0, data }
    }

    pub fn read(&mut self) -> Result<Vec<u8>> {
        self.read_slice_expect(&[31, 139])?;
        ensure!(self.read_u8()? == 8, "compression method is not DEFLATE");

        let flags = self.read_u8()?;
        ensure!(flags & 0b1110_0000 == 0, "invalid gzip flags");
        self.read_slice(6)?; // mtime, extra flags, and the operating system

        if flags & 0b100 != 0 {
            let length = self.read_u16()?;
            self.read_slice(usize::from(length))?;
        }

        for flag in [0b1000, 0b1_0000] {
            if flags & flag != 0 {
                self.read_slice_until(0)?;
            }
        }

        if flags & 0b10 != 0 {
            self.read_slice(2)?;
        }

        let remaining = self.data.len() - self.cursor;
        ensure!(remaining >= 8, "truncated gzip trailer");

        let compressed = self.read_slice(remaining - 8)?;
        self.read_slice(8)?; // CRC32 and uncompressed size

        deflate::inflate(compressed)
    }

    impl_read_slice!();
    impl_read_for_datatype_le!(read_u8, u8);
    impl_read_for_datatype_le!(read_u16, u16);
}
