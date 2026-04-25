//! Deterministic `session_id` derivation for Telegram chats.
//!
//! Matches the WhatsApp plugin pattern: `UUIDv5(NS, chat_id)`. A stable
//! id per chat so reconnects / restarts keep conversation history.

use uuid::Uuid;

const NAMESPACE: Uuid = Uuid::from_u128(0x7b3e_4d9a_5f21_4b73_9c8a_aaaa_bbbb_cccc);

pub fn session_id_for_chat(chat_id: i64) -> Uuid {
    Uuid::new_v5(&NAMESPACE, chat_id.to_string().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deterministic() {
        assert_eq!(session_id_for_chat(12345), session_id_for_chat(12345));
    }
    #[test]
    fn distinct_chats_get_distinct_ids() {
        assert_ne!(session_id_for_chat(1), session_id_for_chat(2));
    }
    #[test]
    fn negative_group_ids_ok() {
        // Telegram uses negative ids for groups/supergroups
        assert_ne!(
            session_id_for_chat(-1001234567890),
            session_id_for_chat(1001234567890)
        );
    }
}
