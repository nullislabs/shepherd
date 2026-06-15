// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: ["../../wit/nexum-host", "../../wit/shepherd-cow"],
    world: "shepherd:cow/shepherd",
    generate_all,
});

use nexum::host::{logging, types};

struct EthFlowWatcher;

impl Guest for EthFlowWatcher {
    fn init(_config: Vec<(String, String)>) -> Result<(), HostError> {
        logging::log(logging::Level::Info, "ethflow-watcher init");
        Ok(())
    }

    fn on_event(event: types::Event) -> Result<(), HostError> {
        // CoWSwapEthFlow `OrderPlacement` decode lands in BLEU-832; the
        // EIP-1271 submission path lands in BLEU-833. Block / Tick /
        // Message are not used by this module.
        if let types::Event::Logs(logs) = event {
            logging::log(
                logging::Level::Info,
                &format!("ethflow received {} logs (decode in BLEU-832)", logs.len()),
            );
        }
        Ok(())
    }
}

export!(EthFlowWatcher);
