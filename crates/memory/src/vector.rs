//! Phase 5.4 — sqlite-vec integration helpers.
//!
//! We register `sqlite3_vec_init` as an SQLite auto-extension the first
//! time `enable()` is called. Every connection opened thereafter in the
//! process auto-loads `vec_version()`, `vec_distance_cosine()`, and the
//! `vec0` virtual-table module.

use std::sync::Once;

static REGISTER_ONCE: Once = Once::new();

/// Register `sqlite3_vec_init` as an auto-extension on the process-wide
/// SQLite runtime. Idempotent — subsequent calls are no-ops.
pub fn enable() {
    REGISTER_ONCE.call_once(|| unsafe {
        // SAFETY: `sqlite3_vec_init` is the loadable-extension entry point
        // for sqlite-vec and matches the `xEntryPoint` signature expected
        // by `sqlite3_auto_extension`.
        type AutoExt = unsafe extern "C" fn(
            *mut libsqlite3_sys::sqlite3,
            *mut *mut std::os::raw::c_char,
            *const libsqlite3_sys::sqlite3_api_routines,
        ) -> std::os::raw::c_int;
        let init: AutoExt = std::mem::transmute(sqlite_vec::sqlite3_vec_init as *const ());
        libsqlite3_sys::sqlite3_auto_extension(Some(init));
    });
}

/// Pack an `f32` vector into the little-endian byte blob that sqlite-vec's
/// `vec0` tables expect.
pub fn pack_f32(vec: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vec.len() * 4);
    for v in vec {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Reverse of `pack_f32`. Returns `None` if `bytes.len() % 4 != 0`.
pub fn unpack_f32(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        let arr = [chunk[0], chunk[1], chunk[2], chunk[3]];
        out.push(f32::from_le_bytes(arr));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_roundtrip() {
        let v = vec![0.1_f32, -2.5, 3.14, f32::MIN, f32::MAX];
        let bytes = pack_f32(&v);
        assert_eq!(bytes.len(), v.len() * 4);
        let back = unpack_f32(&bytes).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn unpack_rejects_non_multiple_of_4() {
        assert!(unpack_f32(&[0u8, 1, 2]).is_none());
    }

    #[tokio::test]
    async fn enable_then_vec_version_query_succeeds() {
        use sqlx::sqlite::SqlitePoolOptions;
        enable();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let row: (String,) = sqlx::query_as("SELECT vec_version()")
            .fetch_one(&pool)
            .await
            .expect("vec_version should be registered");
        assert!(row.0.starts_with("v"), "got {}", row.0);
    }
}
