//! age-encrypted bundle layer.
//!
//! Wraps the bundle body in an `age` stream so the SQLite + git +
//! state files travel encrypted at rest. The manifest (which sits
//! inside the body) goes encrypted along with everything else; the
//! sibling `<bundle>.sha256` covers the encrypted bytes so integrity
//! at the bytes level is still verifiable without the identity.
//!
//! Recipients use the X25519 form (`age1...`). Identities are loaded
//! from a file (one per non-comment line). Both shapes are the
//! canonical age format — we deliberately avoid the SSH-key passthrough
//! to keep the trust boundary explicit.

use std::io::{Read, Write};
use std::path::Path;
use std::str::FromStr;

use age::x25519::{Identity, Recipient};

use crate::error::SnapshotError;

/// Parse a single canonical age recipient string (`age1...`).
pub fn parse_recipient(s: &str) -> Result<Recipient, SnapshotError> {
    Recipient::from_str(s)
        .map_err(|e| SnapshotError::Encryption(format!("recipient parse: {e}")))
}

/// Load every identity (`AGE-SECRET-KEY-1...`) from the given file,
/// skipping blank lines and `#`-comments. Returns at least one
/// identity or [`SnapshotError::Encryption`].
pub fn load_identities(path: &Path) -> Result<Vec<Identity>, SnapshotError> {
    let body = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for raw in body.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let id = Identity::from_str(line).map_err(|e| {
            SnapshotError::Encryption(format!("identity parse failed: {e}"))
        })?;
        out.push(id);
    }
    if out.is_empty() {
        return Err(SnapshotError::Encryption(format!(
            "no identities in {}",
            path.display()
        )));
    }
    Ok(out)
}

/// Wrap `writer` so every byte written is encrypted to the supplied
/// recipients. The returned [`EncryptingWriter`] must be finalized
/// with [`EncryptingWriter::finish`] before the inner writer is
/// closed; dropping the wrapper without calling `finish` produces a
/// truncated bundle that cannot be decrypted.
pub fn encrypt_writer<W: Write>(
    writer: W,
    recipients: Vec<Recipient>,
) -> Result<EncryptingWriter<W>, SnapshotError> {
    if recipients.is_empty() {
        return Err(SnapshotError::Encryption("no recipients".into()));
    }
    let dyn_recipients: Vec<Box<dyn age::Recipient + Send>> = recipients
        .into_iter()
        .map(|r| Box::new(r) as Box<dyn age::Recipient + Send>)
        .collect();
    let encryptor = age::Encryptor::with_recipients(dyn_recipients)
        .ok_or_else(|| SnapshotError::Encryption("encryptor returned None".into()))?;
    let inner = encryptor
        .wrap_output(writer)
        .map_err(|e| SnapshotError::Encryption(format!("wrap_output: {e}")))?;
    Ok(EncryptingWriter { inner: Some(inner) })
}

/// Owning wrapper over an `age::stream::StreamWriter`. Implements
/// [`Write`] for the encryption pipeline and exposes
/// [`EncryptingWriter::finish`] to flush the final block. Without
/// `finish`, the bundle is truncated.
pub struct EncryptingWriter<W: Write> {
    inner: Option<age::stream::StreamWriter<W>>,
}

impl<W: Write> EncryptingWriter<W> {
    /// Flush the trailing block + return the inner writer.
    pub fn finish(mut self) -> Result<W, SnapshotError> {
        let sw = self
            .inner
            .take()
            .expect("stream writer already taken — finish called twice?");
        sw.finish()
            .map_err(|e| SnapshotError::Encryption(format!("finish: {e}")))
    }
}

impl<W: Write> Write for EncryptingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner
            .as_mut()
            .expect("stream writer already taken")
            .write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner
            .as_mut()
            .expect("stream writer already taken")
            .flush()
    }
}

/// Decrypt an age-wrapped reader back to plaintext using the supplied
/// identities.
pub fn decrypt_reader<R: Read + 'static>(
    reader: R,
    identities: &[Identity],
) -> Result<Box<dyn Read>, SnapshotError> {
    let dec = age::Decryptor::new(reader)
        .map_err(|e| SnapshotError::Encryption(format!("decryptor: {e}")))?;
    let recipients_dec = match dec {
        age::Decryptor::Recipients(d) => d,
        age::Decryptor::Passphrase(_) => {
            return Err(SnapshotError::Encryption(
                "passphrase-protected bundles not supported; use age recipients".into(),
            ));
        }
    };
    let id_refs: Vec<&dyn age::Identity> = identities
        .iter()
        .map(|i| i as &dyn age::Identity)
        .collect();
    let inner = recipients_dec
        .decrypt(id_refs.into_iter())
        .map_err(|e| SnapshotError::Encryption(format!("decrypt: {e}")))?;
    Ok(Box::new(inner))
}

/// Short fingerprint for a recipient, embedded in the manifest's
/// `EncryptionMeta` so a reader can confirm which identity decrypts
/// the body without revealing the identity itself.
pub fn fingerprint(recipient: &Recipient) -> String {
    // `age` recipient strings end in a base64-ish tail that already
    // works as a stable fingerprint. We take the last 8 chars as the
    // public-key short form; collisions inside one operator's
    // identity set are vanishingly unlikely and cheap to inspect.
    let s = recipient.to_string();
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let start = n.saturating_sub(8);
    chars[start..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use age::secrecy::ExposeSecret;
    use std::io::{Cursor, Read, Write};

    fn fresh_identity() -> Identity {
        Identity::generate()
    }

    #[test]
    fn parse_recipient_round_trip() {
        let id = fresh_identity();
        let rec = id.to_public();
        let s = rec.to_string();
        let parsed = parse_recipient(&s).unwrap();
        assert_eq!(parsed.to_string(), s);
    }

    #[test]
    fn fingerprint_is_eight_chars() {
        let id = fresh_identity();
        let rec = id.to_public();
        let fp = fingerprint(&rec);
        assert_eq!(fp.chars().count(), 8);
    }

    fn encrypt_to_vec(body: &[u8], rec: Recipient) -> Vec<u8> {
        let mut sink: Vec<u8> = Vec::new();
        let mut w = encrypt_writer(&mut sink, vec![rec]).unwrap();
        w.write_all(body).unwrap();
        w.finish().unwrap();
        sink
    }

    #[test]
    fn encrypt_then_decrypt_round_trips_payload() {
        let id = fresh_identity();
        let rec = id.to_public();
        let body = b"sensitive bundle bytes payload\n";

        let ciphertext = encrypt_to_vec(body, rec);
        assert!(!ciphertext.is_empty());
        assert_ne!(ciphertext, body);

        let mut r = decrypt_reader(Cursor::new(ciphertext), &[id]).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, body);
    }

    #[test]
    fn decrypt_with_wrong_identity_fails() {
        let id_owner = fresh_identity();
        let id_other = fresh_identity();
        let rec = id_owner.to_public();
        let ciphertext = encrypt_to_vec(b"x", rec);
        let res = decrypt_reader(Cursor::new(ciphertext), &[id_other]);
        assert!(res.is_err());
    }

    #[test]
    fn load_identities_skips_comments_and_blanks() {
        let id = fresh_identity();
        let body = format!(
            "# comment\n\n{}\n# trailing comment\n",
            id.to_string().expose_secret()
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.txt");
        std::fs::write(&path, body).unwrap();
        let ids = load_identities(&path).unwrap();
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn load_identities_rejects_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.txt");
        std::fs::write(&path, "# nothing here\n\n").unwrap();
        assert!(load_identities(&path).is_err());
    }

    #[test]
    fn encrypt_with_no_recipients_errors() {
        let mut sink: Vec<u8> = Vec::new();
        match encrypt_writer(&mut sink, Vec::new()) {
            Ok(_) => panic!("expected error from empty recipient set"),
            Err(e) => assert!(format!("{e}").contains("no recipients")),
        }
    }
}
