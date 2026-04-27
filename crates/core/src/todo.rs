//! Phase 79.4 — intra-turn scratch todo list.
//!
//! Distinct from Phase 14 TaskFlow:
//! * **TaskFlow** is persistent, cross-session, DAG-shaped; owned by
//!   the operator + flows.
//! * **Todo** (this module) is in-memory, per-goal, flat list; owned
//!   by the model. Lives on `AgentContext.todos` and dies with the
//!   goal — long Phase 67 driver-loop turns use it to coordinate
//!   sub-steps without spawning sub-goals.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/tools/TodoWriteTool/TodoWriteTool.ts:31-103`
//!     (zero permission checks, full-replace semantics, wipe-on-all-completed,
//!     `oldTodos` + `newTodos` diff in the response).
//!   * `claude-code-leak/src/utils/todo/types.ts:1-19` (`TodoItem`
//!     schema with `content` + `status` + `activeForm`).
//!
//! Reference (secondary):
//!   * OpenClaw — no equivalent. `grep -rln "todo" research/src/` only
//!     surfaces unrelated cron / delivery files.

use serde::{Deserialize, Serialize};

/// Hard cap on items per goal. The leak does not enforce one; nexo-rs
/// adds the cap defensively so a runaway model cannot grow the list
/// unbounded.
pub const MAX_TODO_ITEMS: usize = 50;

/// Per-field UTF-8 byte cap. Keeps a single bad item from saturating
/// the prompt budget on echo. 200 bytes ≈ 50 words, plenty for
/// "Run cargo test --workspace and fix red".
pub const MAX_TODO_FIELD_BYTES: usize = 200;

/// Three-state lifecycle. Mirrors the leak's
/// `TodoStatusSchema = pending | in_progress | completed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// One scratch list item. Lift from
/// `claude-code-leak/src/utils/todo/types.ts:8-15`. The leak has no
/// `id` field — items are identified by position + content. Full
/// replace on every write keeps the model's mental model simple.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    /// Imperative form: "Run tests", "Update docs". Shown in the
    /// list summary.
    pub content: String,
    pub status: TodoStatus,
    /// Present continuous form: "Running tests", "Updating docs".
    /// Rendered while the item is `in_progress` — gives operator
    /// dashboards (and the future pairing companion-tui) a natural
    /// progress string without extra grammar fixup.
    pub active_form: String,
}

/// Convenience alias — the leak's `TodoListSchema = z.array(TodoItemSchema())`
/// is just `Vec<TodoItem>` here, but a named alias keeps signatures
/// across the codebase honest about what kind of `Vec` they hold.
pub type TodoList = Vec<TodoItem>;

/// Validation outcome for a candidate todo write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoValidationError {
    /// More than [`MAX_TODO_ITEMS`] items in the request.
    TooManyItems { actual: usize, max: usize },
    /// `content` or `active_form` empty (after trim) at the named
    /// index.
    EmptyField { index: usize, field: &'static str },
    /// `content` or `active_form` exceeds [`MAX_TODO_FIELD_BYTES`].
    FieldTooLong {
        index: usize,
        field: &'static str,
        actual: usize,
        max: usize,
    },
}

impl std::fmt::Display for TodoValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TodoValidationError::TooManyItems { actual, max } => {
                write!(
                    f,
                    "too many todo items: {actual} > max {max}. Consolidate the list."
                )
            }
            TodoValidationError::EmptyField { index, field } => {
                write!(f, "todo item #{index}: `{field}` is empty")
            }
            TodoValidationError::FieldTooLong {
                index,
                field,
                actual,
                max,
            } => {
                write!(
                    f,
                    "todo item #{index}: `{field}` is {actual} bytes (max {max})"
                )
            }
        }
    }
}

impl std::error::Error for TodoValidationError {}

/// Validate a candidate list against the documented bounds. Returns
/// `Ok(())` when the list is acceptable; the first violation
/// otherwise.
pub fn validate_todos(items: &[TodoItem]) -> Result<(), TodoValidationError> {
    if items.len() > MAX_TODO_ITEMS {
        return Err(TodoValidationError::TooManyItems {
            actual: items.len(),
            max: MAX_TODO_ITEMS,
        });
    }
    for (index, item) in items.iter().enumerate() {
        if item.content.trim().is_empty() {
            return Err(TodoValidationError::EmptyField {
                index,
                field: "content",
            });
        }
        if item.active_form.trim().is_empty() {
            return Err(TodoValidationError::EmptyField {
                index,
                field: "active_form",
            });
        }
        if item.content.len() > MAX_TODO_FIELD_BYTES {
            return Err(TodoValidationError::FieldTooLong {
                index,
                field: "content",
                actual: item.content.len(),
                max: MAX_TODO_FIELD_BYTES,
            });
        }
        if item.active_form.len() > MAX_TODO_FIELD_BYTES {
            return Err(TodoValidationError::FieldTooLong {
                index,
                field: "active_form",
                actual: item.active_form.len(),
                max: MAX_TODO_FIELD_BYTES,
            });
        }
    }
    Ok(())
}

/// `true` when every item has `status == Completed`. Lift from
/// `TodoWriteTool.ts:69-70`. The handler uses this to wipe the list
/// to `[]` so the next planning cycle starts fresh — keeps the
/// scratch surface from accumulating dead items across long turns.
pub fn all_completed(items: &[TodoItem]) -> bool {
    !items.is_empty() && items.iter().all(|t| t.status == TodoStatus::Completed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(content: &str, active: &str) -> TodoItem {
        TodoItem {
            content: content.into(),
            status: TodoStatus::Pending,
            active_form: active.into(),
        }
    }

    #[test]
    fn validate_empty_list_ok() {
        assert!(validate_todos(&[]).is_ok());
    }

    #[test]
    fn validate_normal_item_ok() {
        let items = vec![pending("Run tests", "Running tests")];
        assert!(validate_todos(&items).is_ok());
    }

    #[test]
    fn validate_too_many_items_err() {
        let items: Vec<_> = (0..51).map(|i| pending(&format!("c{i}"), "a")).collect();
        let err = validate_todos(&items).unwrap_err();
        assert!(matches!(
            err,
            TodoValidationError::TooManyItems {
                actual: 51,
                max: 50
            }
        ));
    }

    #[test]
    fn validate_empty_content_err() {
        let items = vec![TodoItem {
            content: "  ".into(),
            status: TodoStatus::Pending,
            active_form: "Doing".into(),
        }];
        let err = validate_todos(&items).unwrap_err();
        assert!(matches!(
            err,
            TodoValidationError::EmptyField {
                index: 0,
                field: "content"
            }
        ));
    }

    #[test]
    fn validate_empty_active_form_err() {
        let items = vec![TodoItem {
            content: "Do".into(),
            status: TodoStatus::Pending,
            active_form: "".into(),
        }];
        let err = validate_todos(&items).unwrap_err();
        assert!(matches!(
            err,
            TodoValidationError::EmptyField {
                index: 0,
                field: "active_form"
            }
        ));
    }

    #[test]
    fn validate_oversized_content_err() {
        let big = "x".repeat(MAX_TODO_FIELD_BYTES + 1);
        let items = vec![pending(&big, "Doing")];
        let err = validate_todos(&items).unwrap_err();
        assert!(matches!(
            err,
            TodoValidationError::FieldTooLong {
                index: 0,
                field: "content",
                ..
            }
        ));
    }

    #[test]
    fn validate_oversized_active_form_err() {
        let big = "x".repeat(MAX_TODO_FIELD_BYTES + 1);
        let items = vec![pending("Do", &big)];
        let err = validate_todos(&items).unwrap_err();
        assert!(matches!(
            err,
            TodoValidationError::FieldTooLong {
                index: 0,
                field: "active_form",
                ..
            }
        ));
    }

    #[test]
    fn all_completed_empty_is_false() {
        assert!(!all_completed(&[]));
    }

    #[test]
    fn all_completed_mixed_is_false() {
        let items = vec![
            TodoItem {
                content: "a".into(),
                status: TodoStatus::Completed,
                active_form: "A".into(),
            },
            TodoItem {
                content: "b".into(),
                status: TodoStatus::Pending,
                active_form: "B".into(),
            },
        ];
        assert!(!all_completed(&items));
    }

    #[test]
    fn all_completed_all_completed_is_true() {
        let items = vec![TodoItem {
            content: "a".into(),
            status: TodoStatus::Completed,
            active_form: "A".into(),
        }];
        assert!(all_completed(&items));
    }

    #[test]
    fn serde_roundtrip_uses_snake_case_status() {
        let item = TodoItem {
            content: "Run".into(),
            status: TodoStatus::InProgress,
            active_form: "Running".into(),
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains(r#""status":"in_progress""#), "got: {json}");
        let back: TodoItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back, item);
    }
}
