use std::convert::TryFrom;

use viz_core::{http, Response};
use viz_utils::serde::json;

/// Responder for a GraphQL response.
///
/// This contains a batch response, but since regular responses are a type of batch response it
/// works for both.
pub struct GraphQLResponse(pub async_graphql::BatchResponse);

impl From<async_graphql::Response> for GraphQLResponse {
    fn from(resp: async_graphql::Response) -> Self {
        Self(resp.into())
    }
}

impl From<async_graphql::BatchResponse> for GraphQLResponse {
    fn from(resp: async_graphql::BatchResponse) -> Self {
        Self(resp)
    }
}

impl From<GraphQLResponse> for Response {
    fn from(gr: GraphQLResponse) -> Self {
        let mut resp = Response::json(Into::<String>::into(json::to_string(&gr.0).unwrap()));
        if gr.0.is_ok() {
            if let Some(cache_control) = gr.0.cache_control().value() {
                if let Ok(value) = http::HeaderValue::from_str(&cache_control) {
                    resp.headers_mut()
                        .insert(http::header::CACHE_CONTROL, value);
                }
            }
        }
        for (name, value) in gr.0.http_headers() {
            if let (Ok(name), Ok(value)) = (
                http::header::HeaderName::try_from(name.as_bytes()),
                http::HeaderValue::from_str(value),
            ) {
                resp.headers_mut().insert(name, value);
            }
        }
        resp
    }
}
