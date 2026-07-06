//! `nexum:host/identity`: deferred to 0.3 (keystore / KMS backend).
//! `accounts()` returns an empty roster so guests can probe-then-skip;
//! signing returns `unsupported`.

use crate::bindings::nexum;
use crate::bindings::nexum::host::types::Fault;
use crate::host::component::RuntimeTypes;
use crate::host::state::HostState;

impl<T: RuntimeTypes> nexum::host::identity::Host for HostState<T> {
    async fn accounts(&mut self) -> Result<Vec<Vec<u8>>, Fault> {
        Ok(vec![])
    }

    async fn sign(&mut self, _account: Vec<u8>, _message: Vec<u8>) -> Result<Vec<u8>, Fault> {
        Err(Fault::Unsupported("sign requires a keystore (0.3)".into()))
    }

    async fn sign_typed_data(
        &mut self,
        _account: Vec<u8>,
        _typed_data: String,
    ) -> Result<Vec<u8>, Fault> {
        Err(Fault::Unsupported(
            "sign-typed-data requires a keystore (0.3)".into(),
        ))
    }
}
