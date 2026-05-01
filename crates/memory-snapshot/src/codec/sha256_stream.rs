//! Streaming SHA-256 reader/writer adapters.
//!
//! Used to seal the bundle in a single pass (no second read of the body)
//! and to verify per-artifact hashes while the tar entries are being
//! produced. Keeps the hash calculation incremental so the runtime never
//! buffers a full bundle in memory.

use std::io::{self, Read, Write};

use sha2::{Digest, Sha256};

/// `Read` adapter that hashes every byte that flows through it.
pub struct HashingReader<R: Read> {
    inner: R,
    hasher: Sha256,
    bytes_read: u64,
}

impl<R: Read> HashingReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            bytes_read: 0,
        }
    }

    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// Consume the reader and return the final lowercase hex digest.
    pub fn finalize_hex(self) -> String {
        format!("{:x}", self.hasher.finalize())
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.hasher.update(&buf[..n]);
            self.bytes_read += n as u64;
        }
        Ok(n)
    }
}

/// `Write` adapter that hashes every byte written.
pub struct HashingWriter<W: Write> {
    inner: W,
    hasher: Sha256,
    bytes_written: u64,
}

impl<W: Write> HashingWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            bytes_written: 0,
        }
    }

    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Lowercase hex digest of the bytes seen so far. Caller can keep
    /// writing after peeking; finalization is non-destructive on
    /// `Sha256::clone`.
    pub fn current_hex(&self) -> String {
        format!("{:x}", self.hasher.clone().finalize())
    }

    pub fn finalize_hex(self) -> (W, String, u64) {
        let digest = format!("{:x}", self.hasher.finalize());
        (self.inner, digest, self.bytes_written)
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        if n > 0 {
            self.hasher.update(&buf[..n]);
            self.bytes_written += n as u64;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// One-shot helper: SHA-256 of an arbitrary byte slice, lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6234 test vector — sha256("abc").
    const ABC_DIGEST: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn one_shot_hex_matches_known_vector() {
        assert_eq!(sha256_hex(b"abc"), ABC_DIGEST);
    }

    #[test]
    fn hashing_reader_streams_match_one_shot() {
        let mut r = HashingReader::new(&b"abc"[..]);
        let mut sink = Vec::new();
        std::io::copy(&mut r, &mut sink).unwrap();
        assert_eq!(sink, b"abc");
        assert_eq!(r.bytes_read(), 3);
        assert_eq!(r.finalize_hex(), ABC_DIGEST);
    }

    #[test]
    fn hashing_reader_chunked_reads_match_full_read() {
        let body: Vec<u8> = (0..1024).map(|i| (i & 0xff) as u8).collect();
        let one_shot = sha256_hex(&body);

        // Force tiny reads to exercise the incremental update path.
        let cursor = std::io::Cursor::new(body.clone());
        let mut r = HashingReader::new(cursor);
        let mut buf = [0u8; 7];
        loop {
            let n = r.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
        }
        assert_eq!(r.bytes_read(), body.len() as u64);
        assert_eq!(r.finalize_hex(), one_shot);
    }

    #[test]
    fn hashing_writer_finalizes_with_inner() {
        let mut w = HashingWriter::new(Vec::<u8>::new());
        w.write_all(b"abc").unwrap();
        let (inner, digest, n) = w.finalize_hex();
        assert_eq!(inner, b"abc");
        assert_eq!(digest, ABC_DIGEST);
        assert_eq!(n, 3);
    }

    #[test]
    fn hashing_writer_current_hex_is_non_destructive() {
        let mut w = HashingWriter::new(Vec::<u8>::new());
        w.write_all(b"ab").unwrap();
        let mid = w.current_hex();
        w.write_all(b"c").unwrap();
        let (_, end, _) = w.finalize_hex();
        // Mid digest is `sha256("ab")`, end is `sha256("abc")` — both
        // valid, distinct, and stable.
        assert_ne!(mid, end);
        assert_eq!(end, ABC_DIGEST);
    }
}
