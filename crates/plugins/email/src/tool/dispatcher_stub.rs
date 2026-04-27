//! Test-only `DispatcherHandle` stub shared across tool unit tests.

#![cfg(test)]

use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use nexo_auth::email::EmailCredentialStore;
use nexo_auth::google::GoogleCredentialStore;
use nexo_config::types::plugins::EmailPluginConfigFile;

use crate::events::OutboundCommand;
use crate::inbound::HealthMap;

use super::context::{DispatcherHandle, EmailToolContext};

pub struct StubDispatcher {
    pub instance_ids_list: Vec<String>,
    pub captured: Mutex<Vec<(String, OutboundCommand)>>,
    pub next_id: Mutex<u32>,
    pub force_err: bool,
}

#[async_trait]
impl DispatcherHandle for StubDispatcher {
    async fn enqueue_for_instance(
        &self,
        instance: &str,
        cmd: OutboundCommand,
    ) -> Result<String> {
        if self.force_err {
            anyhow::bail!("dispatcher rejected for test");
        }
        let mut n = self.next_id.lock().unwrap();
        *n += 1;
        let id = format!("<stub-{n}@test>");
        self.captured.lock().unwrap().push((instance.into(), cmd));
        Ok(id)
    }
    fn instance_ids(&self) -> Vec<String> {
        self.instance_ids_list.clone()
    }
}

pub fn stub_ctx_with_bounce(
    declared: Vec<String>,
    force_err: bool,
    bounce_store: Option<Arc<crate::bounce_store::BounceStore>>,
) -> (Arc<EmailToolContext>, Arc<StubDispatcher>) {
    let yaml = format!(
        "email:\n  accounts:\n{}\n",
        declared
            .iter()
            .map(|i| format!(
                "    - instance: {i}\n      address: {i}@example.com\n      imap: {{ host: imap.x, port: 993 }}\n      smtp: {{ host: smtp.x, port: 587 }}\n"
            ))
            .collect::<String>()
    );
    let f: EmailPluginConfigFile = serde_yaml::from_str(&yaml).unwrap();
    let dispatcher = Arc::new(StubDispatcher {
        instance_ids_list: declared.clone(),
        captured: Mutex::new(vec![]),
        next_id: Mutex::new(0),
        force_err,
    });
    let ctx = Arc::new(EmailToolContext {
        creds: Arc::new(EmailCredentialStore::empty()),
        google: Arc::new(GoogleCredentialStore::empty()),
        config: Arc::new(f.email),
        dispatcher: dispatcher.clone(),
        health: HealthMap::new(DashMap::new().into()),
        bounce_store,
    });
    (ctx, dispatcher)
}

pub fn stub_ctx(
    declared: Vec<String>,
    force_err: bool,
) -> (Arc<EmailToolContext>, Arc<StubDispatcher>) {
    let yaml = format!(
        "email:\n  accounts:\n{}\n",
        declared
            .iter()
            .map(|i| format!(
                "    - instance: {i}\n      address: {i}@example.com\n      imap: {{ host: imap.x, port: 993 }}\n      smtp: {{ host: smtp.x, port: 587 }}\n"
            ))
            .collect::<String>()
    );
    let f: EmailPluginConfigFile = serde_yaml::from_str(&yaml).unwrap();
    let dispatcher = Arc::new(StubDispatcher {
        instance_ids_list: declared.clone(),
        captured: Mutex::new(vec![]),
        next_id: Mutex::new(0),
        force_err,
    });
    let ctx = Arc::new(EmailToolContext {
        creds: Arc::new(EmailCredentialStore::empty()),
        google: Arc::new(GoogleCredentialStore::empty()),
        config: Arc::new(f.email),
        dispatcher: dispatcher.clone(),
        health: HealthMap::new(DashMap::new().into()),
            bounce_store: None,
        });
    (ctx, dispatcher)
}
