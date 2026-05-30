// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: "../../wit/nexum-runtime",
    world: "event-module",
});

use nexum::runtime::logging;
use nexum::runtime::types::{self, HostErrorKind};

struct ExampleModule;

fn module_err(message: impl Into<String>) -> HostError {
    HostError {
        domain: "example".into(),
        kind: HostErrorKind::Internal,
        code: 0,
        message: message.into(),
        data: None,
    }
}

impl Guest for ExampleModule {
    fn init(config: Vec<(String, String)>) -> Result<(), HostError> {
        let name = config
            .iter()
            .find(|(k, _)| k == "name")
            .map(|(_, v)| v.as_str())
            .unwrap_or("unknown");
        logging::log(
            logging::Level::Info,
            &format!("example module init (name={name})"),
        );
        if name.is_empty() {
            return Err(module_err("config 'name' is empty"));
        }
        Ok(())
    }

    fn on_event(event: types::Event) -> Result<(), HostError> {
        match &event {
            types::Event::Block(block) => {
                logging::log(
                    logging::Level::Info,
                    &format!(
                        "block {} on chain {} (ts={}ms)",
                        block.number, block.chain_id, block.timestamp
                    ),
                );
            }
            types::Event::Logs(logs) => {
                logging::log(
                    logging::Level::Info,
                    &format!("received {} log entries", logs.len()),
                );
            }
            types::Event::Tick(tick) => {
                logging::log(
                    logging::Level::Info,
                    &format!("tick fired at {}ms", tick.fired_at),
                );
            }
            types::Event::Message(msg) => {
                logging::log(
                    logging::Level::Info,
                    &format!("message on topic {}", msg.content_topic),
                );
            }
        }
        Ok(())
    }
}

export!(ExampleModule);
