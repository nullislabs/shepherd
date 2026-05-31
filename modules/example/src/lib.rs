// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: "../../wit/nexum-host",
    world: "nexum:host/event-module",
});

use nexum::host::logging;
use nexum::host::types;

struct ExampleModule;

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
