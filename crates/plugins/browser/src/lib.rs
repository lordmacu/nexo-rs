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

use std::sync::Arc;

use nexo_config::BrowserConfig;
use nexo_core::agent::nexo_plugin_registry::PluginFactory;
use nexo_core::agent::plugin_host::NexoPlugin;

/// Phase 81.12.a — factory builder for the browser plugin. Operator
/// (or Phase 81.12.e cleanup) registers this in a
/// `PluginFactoryRegistry` so
/// `wire_plugin_registry(..., Some(&factory))` can instantiate the
/// browser plugin from its manifest. Closure captures `config` by
/// move; each invocation clones `config` to hand the new plugin
/// instance its own copy.
///
/// Today (81.12.a): the factory is exported but no caller registers
/// it — main.rs's legacy block continues to construct `BrowserPlugin`
/// directly. 81.12.e flips main.rs to use this factory.
pub fn browser_plugin_factory(config: BrowserConfig) -> PluginFactory {
    Box::new(move |_manifest| {
        let plugin: Arc<dyn NexoPlugin> = Arc::new(BrowserPlugin::new(config.clone()));
        Ok(plugin)
    })
}
