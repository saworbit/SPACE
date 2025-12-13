pub mod data;
pub mod gossip;
pub mod mesh;
pub mod system;

use crate::api::models::{Meta, RequestContext};

pub(crate) fn with_trace(meta: Meta, ctx: Option<&RequestContext>) -> Meta {
    if let Some(ctx) = ctx {
        meta.with_trace_id(Some(ctx.request_id.clone()))
    } else {
        meta
    }
}
