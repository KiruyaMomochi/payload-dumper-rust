use std::io::{Read, Seek, SeekFrom, Write};

use crate::chromeos_update_engine;

pub struct SectionFile<T> {
    inner: T,
    offset: u64,
    length: u64,

    pos: u64,
}


impl<T: Seek> SectionFile<T> {
    pub fn new(mut inner: T, offset: u64, length: u64) -> std::io::Result<Self> {
        inner.seek(SeekFrom::Start(offset))?;

        Ok(Self {
            inner,
            offset, 
            length,

            pos: 0,
        })
    }

    pub fn new_from_extent(inner: T, extent: chromeos_update_engine::Extent, block_size: u64) -> std::io::Result<Self> {
        Self::new(inner, extent.start_block() * block_size, extent.num_blocks() * block_size)
    }
}

impl<T: Seek> Seek for SectionFile<T> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let pos = match pos {
            SeekFrom::Start(pos) => pos,
            SeekFrom::Current(pos) => (self.pos as i64 + pos) as u64,
            SeekFrom::End(pos) => (self.length as i64 + pos) as u64,
        };

        self.pos = self.inner.seek(SeekFrom::Start(self.offset + pos))? - self.offset;
        Ok(self.pos)
    }
}

impl<T: Read + Seek> Read for SectionFile<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let to_read = std::cmp::min(buf.len() as u64, self.length - self.pos) as usize;
        let read = self.inner.read(&mut buf[..to_read])?;
        self.pos += read as u64;
        Ok(read)
    }
}

impl<T: Write + Seek> Write for SectionFile<T> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let to_write = std::cmp::min(buf.len() as u64, self.length - self.pos) as usize;
        let write = self.inner.write(&buf[..to_write])?;
        self.pos += write as u64;
        Ok(write)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[derive(Debug, Clone)]
pub struct Fragment {
    pub offset: u64,
    pub size: u64,
}

impl Fragment {
    pub fn from_extent(extent: &crate::chromeos_update_engine::Extent, block_size: u64) -> Self {
        Self {
            offset: block_size * extent.start_block(),
            size: block_size * extent.num_blocks(),
        }
    }
}

struct FragmentNode {
    pub offset: u64,
    pub size: u64,
    pub start_pos: u64,
}

pub struct FragmentFile<T> {
    inner: T,
    index: usize,
    fragment_pos: u64,
    size: u64,
    fragments: Vec<FragmentNode>,
}

impl<T: Seek> FragmentFile<T> {
    pub fn new(mut inner: T, fragments: &[Fragment]) -> std::io::Result<Self> {
        if fragments.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Empty fragments",
            ));
        }

        inner.seek(SeekFrom::Start(fragments[0].offset))?;
        let fragments = fragments
            .iter()
            .scan(0u64, |acc, fragment| {
                let ret = FragmentNode {
                    offset: fragment.offset,
                    size: fragment.size,
                    start_pos: *acc,
                };
                *acc += fragment.size;

                Some(ret)
            })
            .collect::<Vec<_>>();

        Ok(Self {
            inner,
            index: 0,
            fragment_pos: 0,
            size: fragments.iter().map(|node| node.size).sum(),
            fragments,
        })
    }

    pub fn new_from_extents(inner: T, extents: &[chromeos_update_engine::Extent], block_size: u64) -> std::io::Result<Self> {
        let fragments: Vec<_> = extents.iter().map(|extent| Fragment::from_extent(extent, block_size)).collect();
        Self::new(inner, &fragments)
    }

    #[inline]
    fn fragment(&self) -> &FragmentNode {
        &self.fragments[self.index]
    }

    #[inline]
    fn fragment_remaining(&self) -> u64 {
        if self.eof() {
            return 0;
        }
        self.fragment().size - self.fragment_pos
    }

    #[inline]
    fn fragment_eof(&self) -> bool {
        self.eof() || self.fragment_remaining() == 0
    }

    #[inline]
    fn eof(&self) -> bool {
        self.index >= self.fragments.len()
    }

    #[inline]
    fn inner_pos(&self) -> u64 {
        self.fragment().offset + self.fragment_pos
    }

    #[inline]
    fn inner_seek(&mut self) -> std::io::Result<u64> {
        let inner_pos = self.inner.seek(SeekFrom::Start(self.inner_pos()))?;
        debug_assert!(inner_pos == self.inner_pos());
        Ok(self.pos())
    }

    #[inline]
    fn next_fragment(&mut self) -> std::io::Result<()> {
        self.index += 1;
        if self.eof() {
            return Ok(());
        }

        self.fragment_pos = 0;
        self.inner_seek()?;
        Ok(())
    }

    #[inline]
    fn pos(&mut self) -> u64 {
        if self.eof() {
            return self.size;
        }
        self.fragment().start_pos + self.fragment_pos
    }
}

impl<T> FragmentFile<T> {
    #[inline]
    pub fn size(&self) -> u64 {
        self.size
    }
}

impl<T: Seek> Seek for FragmentFile<T> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        if pos == SeekFrom::Current(0) {
            return Ok(self.pos());
        }

        let pos = match pos {
            SeekFrom::Start(pos) => pos,
            SeekFrom::Current(pos) => (self.pos() as i64 + pos) as u64,
            SeekFrom::End(pos) => (self.size as i64 + pos) as u64,
        };

        let (index, fragment) = self
            .fragments
            .iter()
            .enumerate()
            .take_while(|(_, FragmentNode { start_pos, .. })| start_pos <= &pos)
            .last()
            .unwrap();
            // .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid seek"))?;

        self.index = index;
        self.fragment_pos = pos - fragment.start_pos;
        self.inner_seek()
    }
}

impl<T: Seek + Read> Read for FragmentFile<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut read = 0;
        while read < buf.len() && !self.eof() {
            let to_read = std::cmp::min(self.fragment_remaining() as usize, buf.len() - read);
            let read_now = self.inner.read(&mut buf[read..read + to_read])?;
            read += read_now;
            self.fragment_pos += read_now as u64;

            if self.fragment_eof() {
                self.next_fragment()?;
            }
        }

        Ok(read)
    }
}

impl<T: Seek + Write> Write for FragmentFile<T> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut written = 0;
        while written < buf.len() && !self.eof() {
            let to_write = std::cmp::min(self.fragment_remaining() as usize, buf.len() - written);
            let written_now = self.inner.write(&buf[written..written + to_write])?;
            written += written_now;
            self.fragment_pos += written_now as u64;

            if self.fragment_eof() {
                self.next_fragment()?;
            }
        }

        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn fragment() -> std::io::Result<()> {
        let mut vec = (0..31).collect::<Vec<_>>();
        let cursor = Cursor::new(&mut vec);
        let fragments = vec![
            Fragment { offset: 0, size: 5 },
            Fragment {
                offset: 20,
                size: 2,
            },
            Fragment {
                offset: 10,
                size: 3,
            },
        ];
        let mut fvec = FragmentFile::new(cursor, &fragments)?;

        let mut buf = vec![0; 20];
        let read = fvec.read(&mut buf)?;
        assert_eq!(read, 10);
        println!("{:?}", buf);

        fvec.seek(SeekFrom::Start(0))?;
        let written = fvec.write(&[9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 0])?;
        assert_eq!(written, 10);
        assert_eq!(&vec[0..5], &[9, 8, 7, 6, 5]);
        assert_eq!(&vec[20..22], &[4, 3]);
        assert_eq!(&vec[10..13], &[2, 1, 0]);

        Ok(())
    }
}
