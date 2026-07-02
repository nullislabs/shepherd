//! `nexum:host/identity`: deferred to 0.3 (keystore / KMS backend).
//! `accounts()` returns an empty roster so guests can probe-then-skip;
//! signing returns `Unsupported`.

use crate::bindings::HostError;
use crate::bindings::nexum;
use crate::host::error::unimplemented;
use crate::host::state::HostState;

impl nexum::host::identity::Host for HostState {
    async fn accounts(&mut self) -> Result<Vec<Vec<u8>>, HostError> {
        Ok(vec![])
    }

    async fn sign(&mut self, _account: Vec<u8>, _message: Vec<u8>) -> Result<Vec<u8>, HostError> {
        Err(unimplemented("identity", "sign requires a keystore (0.3)"))
    }

    async fn sign_typed_data(
        &mut self,
        _account: Vec<u8>,
        _typed_data: String,
    ) -> Result<Vec<u8>, HostError> {
        Err(unimplemented(
            "identity",
            "sign-typed-data requires a keystore (0.3)",
        ))
    }
}
