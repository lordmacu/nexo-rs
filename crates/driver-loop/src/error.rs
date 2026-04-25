use thiserror::Error;

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("config: {0}")]
    Config(String),
    #[error("workspace: {0}")]
    Workspace(String),
    #[error("workspace path escapes root: {path}")]
    WorkspaceTraversal { path: String },
    #[error("harness: {0}")]
    Harness(#[from] nexo_driver_types::HarnessError),
    #[error("claude: {0}")]
    Claude(#[from] nexo_driver_claude::ClaudeError),
    #[error("permission: {0}")]
    Permission(#[from] nexo_driver_permission::PermissionError),
    #[error("acceptance: {0}")]
    Acceptance(String),
    #[error("socket: {0}")]
    Socket(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("yaml: {0}")]
    Yaml(String),
    #[error("nats: {0}")]
    Nats(String),
    #[error("{0}")]
    Other(String),
}
