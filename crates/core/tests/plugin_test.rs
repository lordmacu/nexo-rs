use std::sync::{atomic::Ordering, Arc};

use agent_broker::AnyBroker;
use agent_core::agent::{Command, MockPlugin, Plugin, PluginRegistry, Response};
use async_trait::async_trait;

#[tokio::test]
async fn mock_plugin_start_stop() {
    let plugin = MockPlugin::new("whatsapp");
    let started = Arc::clone(&plugin.started);
    let stopped = Arc::clone(&plugin.stopped);

    let broker = AnyBroker::local();
    plugin.start(broker).await.unwrap();
    assert!(started.load(Ordering::SeqCst));

    plugin.stop().await.unwrap();
    assert!(stopped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn mock_plugin_send_command_records_in_order() {
    let plugin = MockPlugin::new("telegram");
    let received = Arc::clone(&plugin.received);

    let cmd1 = Command::SendMessage { to: "+57300".to_string(), text: "hello".to_string() };
    let cmd2 = Command::SendMessage { to: "+57300".to_string(), text: "world".to_string() };

    plugin.send_command(cmd1).await.unwrap();
    plugin.send_command(cmd2).await.unwrap();

    let cmds = received.lock().unwrap();
    assert_eq!(cmds.len(), 2);
    match &cmds[0] {
        Command::SendMessage { text, .. } => assert_eq!(text, "hello"),
        _ => panic!("wrong command"),
    }
    match &cmds[1] {
        Command::SendMessage { text, .. } => assert_eq!(text, "world"),
        _ => panic!("wrong command"),
    }
}

#[tokio::test]
async fn mock_plugin_with_response() {
    let expected = Response::MessageSent { message_id: "msg-42".to_string() };
    let plugin = MockPlugin::with_response("browser", expected.clone());

    let cmd = Command::SendMessage { to: "user".to_string(), text: "hi".to_string() };
    let resp = plugin.send_command(cmd).await.unwrap();

    match resp {
        Response::MessageSent { message_id } => assert_eq!(message_id, "msg-42"),
        _ => panic!("unexpected response"),
    }
}

#[tokio::test]
async fn registry_get_missing() {
    let registry = PluginRegistry::new();
    assert!(registry.get("nonexistent").is_none());
}

#[tokio::test]
async fn registry_names() {
    let registry = PluginRegistry::new();
    registry.register(MockPlugin::new("whatsapp"));
    registry.register(MockPlugin::new("telegram"));

    let mut names = registry.names();
    names.sort();
    assert_eq!(names, vec!["telegram", "whatsapp"]);
}

#[tokio::test]
async fn registry_start_stop_all() {
    let p1 = MockPlugin::new("whatsapp");
    let p2 = MockPlugin::new("telegram");
    let started1 = Arc::clone(&p1.started);
    let started2 = Arc::clone(&p2.started);
    let stopped1 = Arc::clone(&p1.stopped);
    let stopped2 = Arc::clone(&p2.stopped);

    let registry = PluginRegistry::new();
    registry.register(p1);
    registry.register(p2);

    let broker = AnyBroker::local();
    registry.start_all(broker).await.unwrap();
    assert!(started1.load(Ordering::SeqCst));
    assert!(started2.load(Ordering::SeqCst));

    registry.stop_all().await.unwrap();
    assert!(stopped1.load(Ordering::SeqCst));
    assert!(stopped2.load(Ordering::SeqCst));
}

#[tokio::test]
async fn stop_all_continues_on_error() {
    struct FailingPlugin;

    #[async_trait]
    impl Plugin for FailingPlugin {
        fn name(&self) -> &str { "failing" }
        async fn start(&self, _broker: AnyBroker) -> anyhow::Result<()> { Ok(()) }
        async fn stop(&self) -> anyhow::Result<()> {
            anyhow::bail!("intentional stop failure")
        }
        async fn send_command(&self, _cmd: Command) -> anyhow::Result<Response> {
            Ok(Response::Ok)
        }
    }

    let good = MockPlugin::new("good");
    let stopped_good = Arc::clone(&good.stopped);

    let registry = PluginRegistry::new();
    registry.register(FailingPlugin);
    registry.register(good);

    // stop_all must return Ok even when one plugin fails
    let result = registry.stop_all().await;
    assert!(result.is_ok());

    // The good plugin must still have been stopped
    assert!(stopped_good.load(Ordering::SeqCst));
}
