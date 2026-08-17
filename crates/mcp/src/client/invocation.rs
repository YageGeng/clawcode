//! Correlates Server-initiated Host callbacks with in-flight agent requests.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use protocol::McpRequestContext;
use rmcp::model::RequestId;
use rmcp::service::{InboundStreamOrigin, RequestContext, RoleClient};

/// One request that is registered before transport dispatch to avoid callback races.
struct InvocationRecord {
    request_id: Option<RequestId>,
    context: McpRequestContext,
}

/// Shared registry storage retained by both the handler and request leases.
struct InvocationRegistryInner {
    next_token: AtomicU64,
    records: RwLock<BTreeMap<u64, InvocationRecord>>,
}

/// Resolves nested Host requests to exact Turn and Trace correlation.
#[derive(Clone)]
pub(super) struct InvocationRegistry {
    inner: Arc<InvocationRegistryInner>,
}

impl InvocationRegistry {
    /// Creates an empty registry shared by one connected MCP client.
    pub(super) fn new() -> Self {
        Self {
            inner: Arc::new(InvocationRegistryInner {
                next_token: AtomicU64::new(1),
                records: RwLock::new(BTreeMap::new()),
            }),
        }
    }

    /// Registers correlation before request dispatch and returns its cleanup lease.
    pub(super) fn begin(&self, context: McpRequestContext) -> InvocationLease {
        let token = self.inner.next_token.fetch_add(1, Ordering::Relaxed);
        self.inner
            .records
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                token,
                InvocationRecord {
                    request_id: None,
                    context,
                },
            );
        InvocationLease {
            token,
            inner: Arc::clone(&self.inner),
        }
    }

    /// Resolves stream-associated requests exactly and coarse transports only when unambiguous.
    pub(super) fn resolve(
        &self,
        request: &RequestContext<RoleClient>,
    ) -> Option<McpRequestContext> {
        let records = self
            .inner
            .records
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match request.extensions.get::<InboundStreamOrigin>() {
            Some(InboundStreamOrigin::Unassociated) => None,
            Some(InboundStreamOrigin::OutboundRequest(request_id)) => records
                .values()
                .find(|record| record.request_id.as_ref() == Some(request_id))
                .map(|record| record.context.clone())
                .or_else(|| Self::unique_context(&records)),
            None => Self::unique_context(&records),
        }
    }

    /// Returns the only active context for transports without stream association.
    fn unique_context(
        records: &BTreeMap<u64, InvocationRecord>,
    ) -> Option<McpRequestContext> {
        let mut values = records.values();
        let context = values.next()?.context.clone();
        values.next().is_none().then_some(context)
    }
}

/// Removes request correlation when the originating call settles for any reason.
pub(super) struct InvocationLease {
    token: u64,
    inner: Arc<InvocationRegistryInner>,
}

impl InvocationLease {
    /// Associates the transport-generated request identifier after dispatch completes.
    pub(super) fn associate(&self, request_id: RequestId) {
        if let Some(record) = self
            .inner
            .records
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&self.token)
        {
            record.request_id = Some(request_id);
        }
    }
}

impl Drop for InvocationLease {
    /// Removes this invocation without requiring asynchronous cleanup paths.
    fn drop(&mut self) {
        if let Ok(mut records) = self.inner.records.write() {
            records.remove(&self.token);
        }
    }
}
