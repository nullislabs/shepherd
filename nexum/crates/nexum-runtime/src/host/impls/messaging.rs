//! `nexum:host/messaging`: stub. `publish` reports `unsupported`, `query`
//! returns empty; the per-store topic scope is still enforced ahead of the
//! stub.

use crate::bindings::nexum;
use crate::bindings::nexum::host::types::Fault;
use crate::host::component::RuntimeTypes;
use crate::host::state::HostState;

/// Whether `topic` falls within `scope`. Empty scope admits everything;
/// otherwise a topic matches a scope entry exactly or as a `/`-bounded path
/// prefix, so a grant never leaks into a longer sibling segment.
fn topic_in_scope(topic: &str, scope: &[String]) -> bool {
    if scope.is_empty() {
        return true;
    }
    scope.iter().any(|allowed| {
        if topic == allowed {
            return true;
        }
        let prefix = allowed.strip_suffix('/').unwrap_or(allowed);
        topic
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
    })
}

impl<T: RuntimeTypes> nexum::host::messaging::Host for HostState<T> {
    async fn publish(&mut self, content_topic: String, _payload: Vec<u8>) -> Result<(), Fault> {
        if !topic_in_scope(&content_topic, &self.messaging_topics) {
            return Err(Fault::Denied(format!(
                "content topic {content_topic:?} outside this component's messaging scope"
            )));
        }
        Err(Fault::Unsupported("Waku backend deferred to 0.3".into()))
    }

    async fn query(
        &mut self,
        _content_topic: String,
        _start_time: Option<u64>,
        _end_time: Option<u64>,
        _limit: Option<u32>,
    ) -> Result<Vec<nexum::host::types::Message>, Fault> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::topic_in_scope;

    #[test]
    fn empty_scope_admits_everything() {
        assert!(topic_in_scope("/nexum/1/anything/proto", &[]));
    }

    #[test]
    fn exact_topic_is_admitted() {
        let scope = vec!["/nexum/1/acme-orders/proto".to_owned()];
        assert!(topic_in_scope("/nexum/1/acme-orders/proto", &scope));
        assert!(!topic_in_scope("/nexum/1/other/proto", &scope));
    }

    #[test]
    fn prefix_scope_admits_the_family_but_not_a_sibling() {
        let scope = vec!["/nexum/1/".to_owned()];
        assert!(topic_in_scope("/nexum/1/acme-orders/proto", &scope));
        assert!(topic_in_scope("/nexum/1/twap/proto", &scope));
        // A sibling namespace stays out.
        assert!(!topic_in_scope("/nexum/2/acme-orders/proto", &scope));
    }

    #[test]
    fn prefix_boundary_is_a_path_segment_not_a_substring() {
        // A scope entry without a trailing slash still bounds on the path
        // separator, so it cannot leak into a longer sibling segment.
        let scope = vec!["/nexum/1/acme".to_owned()];
        assert!(topic_in_scope("/nexum/1/acme", &scope));
        assert!(topic_in_scope("/nexum/1/acme/orders", &scope));
        assert!(!topic_in_scope("/nexum/1/acme-orders/proto", &scope));
    }
}
