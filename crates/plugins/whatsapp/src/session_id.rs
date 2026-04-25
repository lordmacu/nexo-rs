//! Deterministic `session_id` derivation for WhatsApp chats.
//!
//! The broker carries a `session_id: Option<Uuid>` on every event so the
//! core runtime can route multiple inbound messages to the same session
//! debounce queue. We derive it as `UUIDv5(NS, bare_jid)` so:
//!
//! * Restarts of the plugin keep the same session identity per chat.
//! * Different plugin installs over the same account still share session
//!   ids (handy for multi-process setups).
//! * A DM and a group chat never collide because the JID already encodes
//!   the domain (`@s.whatsapp.net` vs `@g.us`).

use uuid::Uuid;

/// Project-local namespace for WhatsApp session ids. Pinned — changing
/// this invalidates every persisted conversation.
const NAMESPACE: Uuid = Uuid::from_u128(0x7b3e_4d9a_5f21_4b73_9c8a_1122_3344_5566);

/// Strip the Signal device suffix (`:N`) from a JID before hashing.
fn bare(jid: &str) -> &str {
    match (jid.find(':'), jid.find('@')) {
        (Some(colon), Some(at)) if colon < at => {
            // "user:device@domain" → "user@domain" — rebuild cheaply by
            // returning a slice trick; caller keeps a short-lived String.
            // (We don't allocate here — callers go through `bare_string`.)
            jid
        }
        _ => jid,
    }
}

fn bare_string(jid: &str) -> String {
    if let (Some(colon), Some(at)) = (jid.find(':'), jid.find('@')) {
        if colon < at {
            let mut s = String::with_capacity(jid.len() - (at - colon));
            s.push_str(&jid[..colon]);
            s.push_str(&jid[at..]);
            return s;
        }
    }
    let _ = bare; // keep symbol to document intent
    jid.to_string()
}

/// Derive the session id for a given WhatsApp JID.
///
/// **Note:** this overload predates multi-instance support and hashes
/// only the bare JID. When two WhatsApp accounts (e.g. ana and kate)
/// receive a message from the same external JID, they produce the
/// same session id — dedup collisions can drop one of them. Prefer
/// [`session_id_for_jid_in_instance`] in new code; this one stays
/// for back-compat with code paths that don't have an instance
/// label in scope yet.
pub fn session_id_for_jid(jid: &str) -> Uuid {
    Uuid::new_v5(&NAMESPACE, bare_string(jid).as_bytes())
}

/// Instance-scoped session id. Use this whenever the caller knows
/// which WhatsApp instance ("ana" / "kate" / etc.) handled the
/// message — two accounts receiving from the same JID get distinct
/// session ids.
pub fn session_id_for_jid_in_instance(instance: &str, jid: &str) -> Uuid {
    let key = format!("{}.{}", instance, bare_string(jid));
    Uuid::new_v5(&NAMESPACE, key.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_device_suffix() {
        let a = session_id_for_jid("573111111111:20@s.whatsapp.net");
        let b = session_id_for_jid("573111111111@s.whatsapp.net");
        assert_eq!(a, b, "device suffix must not change session identity");
    }

    #[test]
    fn different_jids_produce_different_ids() {
        let a = session_id_for_jid("573111111111@s.whatsapp.net");
        let b = session_id_for_jid("573222222222@s.whatsapp.net");
        assert_ne!(a, b);
    }

    #[test]
    fn group_and_dm_do_not_collide() {
        let dm = session_id_for_jid("573111111111@s.whatsapp.net");
        let g = session_id_for_jid("573111111111@g.us");
        assert_ne!(dm, g);
    }

    #[test]
    fn deterministic_across_calls() {
        let jid = "573999999999:3@s.whatsapp.net";
        assert_eq!(session_id_for_jid(jid), session_id_for_jid(jid));
    }

    #[test]
    fn instance_scoped_ids_differ_across_instances() {
        let jid = "573111111111@s.whatsapp.net";
        let ana = session_id_for_jid_in_instance("ana", jid);
        let kate = session_id_for_jid_in_instance("kate", jid);
        assert_ne!(ana, kate, "two accounts must not collide on the same JID");
    }

    #[test]
    fn instance_scoped_strips_device_suffix() {
        let with_dev = session_id_for_jid_in_instance("ana", "5731:20@s.whatsapp.net");
        let bare = session_id_for_jid_in_instance("ana", "5731@s.whatsapp.net");
        assert_eq!(with_dev, bare);
    }

    #[test]
    fn instance_scoped_deterministic() {
        let jid = "5731@s.whatsapp.net";
        assert_eq!(
            session_id_for_jid_in_instance("ana", jid),
            session_id_for_jid_in_instance("ana", jid)
        );
    }
}
