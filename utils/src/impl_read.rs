#[macro_export]
macro_rules! impl_read_for_datatype_be {
    ($name:ident, $type:ty) => {
        #[allow(dead_code)]
        fn $name(&mut self) -> ::anyhow::Result<$type> {
            let width = ::std::mem::size_of::<$type>();
            let slice = self.read_slice(width)?;

            Ok(<$type>::from_be_bytes(slice.try_into()?))
        }
    };
}

#[macro_export]
macro_rules! impl_read_for_datatype_le {
    ($name:ident, $type:ty) => {
        #[allow(dead_code)]
        fn $name(&mut self) -> ::anyhow::Result<$type> {
            let width = ::std::mem::size_of::<$type>();
            let slice = self.read_slice(width)?;

            Ok(<$type>::from_le_bytes(slice.try_into()?))
        }
    };
}

#[macro_export]
macro_rules! impl_read_slice {
    () => {
        #[allow(dead_code)]
        fn read_vec<T>(
            &mut self,
            capacity: usize,
            read_fn: impl Fn(&mut Self) -> ::anyhow::Result<T>,
        ) -> ::anyhow::Result<Vec<T>> {
            (0..capacity)
                .map(|_| read_fn(self))
                .collect::<::anyhow::Result<Vec<_>>>()
        }

        #[allow(dead_code)]
        fn read_array<const N: usize>(&mut self) -> ::anyhow::Result<[u8; N]> {
            let bs = self.read_slice(N)?;

            bs.try_into().map_err(|e| ::anyhow::anyhow!("{e:?}"))
        }

        #[allow(dead_code)]
        fn read_slice(&mut self, len: usize) -> ::anyhow::Result<&'a [u8]> {
            let slice = self.peek_slice(len)?;

            self.cursor += len;

            Ok(slice)
        }

        #[allow(dead_code)]
        fn peek_slice(&self, len: usize) -> ::anyhow::Result<&'a [u8]> {
            let end = self
                .cursor
                .checked_add(len)
                .ok_or_else(|| ::anyhow::anyhow!("oob"))?;

            self.data
                .get(self.cursor..end)
                .ok_or_else(|| ::anyhow::anyhow!("oob"))
        }

        #[allow(dead_code)]
        fn read_slice_until(&mut self, term: u8) -> ::anyhow::Result<&'a [u8]> {
            let len = self
                .data
                .get(self.cursor..)
                .and_then(|rest| rest.iter().position(|&b| b == term))
                .ok_or_else(|| ::anyhow::anyhow!("terminator not found"))?;

            let slice = self.read_slice(len)?;

            // consume the term byte
            self.cursor += 1;

            Ok(slice)
        }

        #[allow(dead_code)]
        fn read_slice_expect(&mut self, expect: &[u8]) -> ::anyhow::Result<&'a [u8]> {
            let bs = self.read_slice(expect.len())?;

            ::anyhow::ensure!(bs == expect, "expected {expect:?}, got: {bs:?}");

            Ok(bs)
        }
    };
}
