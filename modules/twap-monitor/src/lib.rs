// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: ["../../wit/nexum-host", "../../wit/shepherd-cow"],
    world: "shepherd:cow/shepherd",
    generate_all,
});

use nexum::host::logging;
use nexum::host::types;

struct TwapMonitor;

impl Guest for TwapMonitor {
    fn init(_config: Vec<(String, String)>) -> Result<(), HostError> {
        logging::log(logging::Level::Info, "twap-monitor init");
        Ok(())
    }

    fn on_event(_event: types::Event) -> Result<(), HostError> {
        // Dispatch on Event::Log (ConditionalOrderCreated) and Event::Block
        // (TWAP poll tick) lands in BLEU-826 / BLEU-827.
        Ok(())
    }
}

export!(TwapMonitor);
