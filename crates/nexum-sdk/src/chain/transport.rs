//! [`HostTransport`]: the alloy transport over [`ChainHost::request`].

use std::future::ready;
use std::task::{Context, Poll};

use alloy_json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy_transport::{TransportError, TransportErrorKind, TransportFut};
use serde_json::value::RawValue;
use tower::Service;

use super::{Chain, ChainMethod};
use crate::host::{ChainError, ChainHost};

/// An alloy `Transport` routing JSON-RPC through the host's chain
/// interface. Dispatch is synchronous: the host blocks the guest until
/// the response is available, so every returned future is ready on its
/// first poll and [`block_on`](super::block_on) drives it for free.
///
/// Methods outside the typed [`ChainMethod`] surface never reach the
/// host; they fail as a JSON-RPC `-32601` error response. A structured
/// node error comes back as the error payload (code, message, revert
/// bytes as `0x` hex); a host [`Fault`](crate::host::Fault) surfaces as
/// a custom transport error carrying the typed fault.
#[derive(Clone, Copy, Debug)]
pub struct HostTransport<H> {
    host: H,
    chain: Chain,
}

impl<H> HostTransport<H>
where
    H: ChainHost + Clone + Send + Sync + 'static,
{
    /// Transport dispatching on `chain` through `host`.
    pub const fn new(host: H, chain: Chain) -> Self {
        Self { host, chain }
    }

    fn dispatch(&self, packet: RequestPacket) -> Result<ResponsePacket, TransportError> {
        match packet {
            RequestPacket::Single(req) => Ok(ResponsePacket::Single(self.dispatch_single(&req)?)),
            RequestPacket::Batch(reqs) => reqs
                .iter()
                .map(|req| self.dispatch_single(req))
                .collect::<Result<Vec<_>, _>>()
                .map(ResponsePacket::Batch),
        }
    }

    fn dispatch_single(&self, req: &SerializedRequest) -> Result<Response, TransportError> {
        let Ok(method) = ChainMethod::try_from(req.method()) else {
            return Ok(failure(
                req,
                ErrorPayload {
                    code: -32601,
                    message: format!(
                        "method outside the permitted read surface: {}",
                        req.method()
                    )
                    .into(),
                    data: None,
                },
            ));
        };
        let params = req.params().map_or("[]", RawValue::get);
        match self
            .host
            .request(self.chain.into(), method.as_str(), params)
        {
            Ok(result) => {
                let payload = RawValue::from_string(result)
                    .map_err(|e| TransportError::deser_err(e, "host chain response"))?;
                Ok(Response {
                    id: req.id().clone(),
                    payload: ResponsePayload::Success(payload),
                })
            }
            Err(ChainError::Rpc(rpc)) => Ok(failure(
                req,
                ErrorPayload {
                    code: rpc.code.into(),
                    message: rpc.message.into(),
                    data: rpc.data.and_then(|bytes| {
                        serde_json::value::to_raw_value(&alloy_primitives::hex::encode_prefixed(
                            bytes,
                        ))
                        .ok()
                    }),
                },
            )),
            Err(ChainError::Fault(fault)) => Err(TransportErrorKind::custom(fault)),
        }
    }
}

fn failure(req: &SerializedRequest, payload: ErrorPayload) -> Response {
    Response {
        id: req.id().clone(),
        payload: ResponsePayload::Failure(payload),
    }
}

impl<H> Service<RequestPacket> for HostTransport<H>
where
    H: ChainHost + Clone + Send + Sync + 'static,
{
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, packet: RequestPacket) -> Self::Future {
        let result = self.dispatch(packet);
        Box::pin(ready(result))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use alloy_json_rpc::{Id, Request, RequestPacket, ResponsePacket, ResponsePayload};
    use alloy_transport::TransportError;
    use tower::Service;

    use super::HostTransport;
    use crate::chain::{Chain, block_on};
    use crate::host::{ChainError, ChainHost, Fault, RpcError};

    type StubFn = dyn Fn(u64, &str, &str) -> Result<String, ChainError> + Send + Sync;

    #[derive(Clone)]
    struct Stub(Arc<StubFn>);

    impl Stub {
        fn new(
            f: impl Fn(u64, &str, &str) -> Result<String, ChainError> + Send + Sync + 'static,
        ) -> Self {
            Self(Arc::new(f))
        }
    }

    impl ChainHost for Stub {
        fn request(&self, chain_id: u64, method: &str, params: &str) -> Result<String, ChainError> {
            (self.0)(chain_id, method, params)
        }
    }

    fn single(method: &'static str) -> RequestPacket {
        let req = Request::new(method, Id::Number(1), ())
            .serialize()
            .expect("request serializes");
        RequestPacket::Single(req)
    }

    fn call(transport: &mut HostTransport<Stub>, packet: RequestPacket) -> super::Response {
        let ResponsePacket::Single(resp) =
            block_on(Service::call(transport, packet)).expect("transport dispatches")
        else {
            panic!("single request yields a single response");
        };
        resp
    }

    #[test]
    fn success_passes_host_json_through() {
        let stub = Stub::new(|chain_id, method, params| {
            assert_eq!(chain_id, 100);
            assert_eq!(method, "eth_blockNumber");
            assert_eq!(params, "[]");
            Ok("\"0x2a\"".into())
        });
        let mut transport = HostTransport::new(stub, Chain::from_id(100));
        let resp = call(&mut transport, single("eth_blockNumber"));
        let ResponsePayload::Success(payload) = resp.payload else {
            panic!("expected success, got {resp:?}");
        };
        assert_eq!(payload.get(), "\"0x2a\"");
    }

    #[test]
    fn unlisted_method_never_reaches_the_host() {
        let stub = Stub::new(|_, method, _| panic!("host must not see {method}"));
        let mut transport = HostTransport::new(stub, Chain::mainnet());
        let resp = call(&mut transport, single("eth_sendRawTransaction"));
        let ResponsePayload::Failure(err) = resp.payload else {
            panic!("expected failure, got {resp:?}");
        };
        assert_eq!(err.code, -32601);
        assert!(err.message.contains("eth_sendRawTransaction"));
    }

    #[test]
    fn rpc_error_surfaces_code_message_and_revert_hex() {
        let stub = Stub::new(|_, _, _| {
            Err(ChainError::Rpc(RpcError {
                code: -32000,
                message: "execution reverted".into(),
                data: Some(vec![0x08, 0xc3, 0x79, 0xa0].into()),
            }))
        });
        let mut transport = HostTransport::new(stub, Chain::mainnet());
        let resp = call(&mut transport, single("eth_call"));
        let ResponsePayload::Failure(err) = resp.payload else {
            panic!("expected failure, got {resp:?}");
        };
        assert_eq!(err.code, -32000);
        assert_eq!(err.message, "execution reverted");
        assert_eq!(err.data.expect("revert data").get(), "\"0x08c379a0\"",);
    }

    #[test]
    fn fault_becomes_a_typed_transport_error() {
        let stub = Stub::new(|_, _, _| Err(ChainError::Fault(Fault::Timeout)));
        let mut transport = HostTransport::new(stub, Chain::mainnet());
        let err = block_on(Service::call(&mut transport, single("eth_call")))
            .expect_err("fault propagates");
        let TransportError::Transport(kind) = err else {
            panic!("expected transport kind, got {err:?}");
        };
        assert!(kind.to_string().contains("timeout"));
    }

    #[test]
    fn batches_dispatch_per_request() {
        let stub = Stub::new(|_, method, _| match method {
            "eth_blockNumber" => Ok("\"0x1\"".into()),
            _ => Ok("\"0x64\"".into()),
        });
        let mut transport = HostTransport::new(stub, Chain::mainnet());
        let reqs = vec![
            Request::new("eth_blockNumber", Id::Number(1), ())
                .serialize()
                .expect("request serializes"),
            Request::new("eth_chainId", Id::Number(2), ())
                .serialize()
                .expect("request serializes"),
        ];
        let ResponsePacket::Batch(resps) =
            block_on(Service::call(&mut transport, RequestPacket::Batch(reqs)))
                .expect("batch dispatches")
        else {
            panic!("batch request yields a batch response");
        };
        assert_eq!(resps.len(), 2);
    }
}
