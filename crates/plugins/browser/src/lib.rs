pub mod cdp;
pub mod chrome;
pub mod command;
pub mod plugin;
pub mod tool;

pub use cdp::{CdpClient, CdpSession};
pub use chrome::{ChromeLauncher, RunningChrome};
pub use command::{BrowserCmd, BrowserResult};
pub use plugin::BrowserPlugin;
pub use tool::{
    register_all as register_browser_tools, BrowserClickTool, BrowserCurrentUrlTool,
    BrowserEvaluateTool, BrowserFillTool, BrowserGoBackTool, BrowserGoForwardTool,
    BrowserNavigateTool, BrowserPressKeyTool, BrowserScreenshotTool, BrowserScrollToTool,
    BrowserSnapshotTool, BrowserWaitForTool,
};
