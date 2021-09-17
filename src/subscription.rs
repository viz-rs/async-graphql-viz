use std::{borrow::Cow, future::Future};

use async_graphql::{
    http::{WebSocketProtocols, WsMessage},
    Data, ObjectType, Result, Schema, SubscriptionType,
};

use viz_core::{
    http::{
        header,
        headers::{self, Header, HeaderName, HeaderValue},
    },
    ws::{Message, WebSocket},
};
use viz_utils::{
    futures::{future, SinkExt, StreamExt},
    serde::json::Value,
};

/// The Sec-Websocket-Protocol header.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct SecWebsocketProtocol(pub WebSocketProtocols);

impl Header for SecWebsocketProtocol {
    fn name() -> &'static HeaderName {
        &header::SEC_WEBSOCKET_PROTOCOL
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
    where
        Self: Sized,
        I: Iterator<Item = &'i HeaderValue>,
    {
        match values.next() {
            Some(value) => Ok(SecWebsocketProtocol(
                value
                    .to_str()
                    .map_err(|_| headers::Error::invalid())?
                    .parse()
                    .ok()
                    .unwrap_or(WebSocketProtocols::SubscriptionsTransportWS),
            )),
            None => Err(headers::Error::invalid()),
        }
    }

    fn encode<E: Extend<HeaderValue>>(&self, values: &mut E) {
        values.extend(std::iter::once(HeaderValue::from_static(
            self.0.sec_websocket_protocol(),
        )))
    }
}

/// GraphQL subscription handler
pub async fn graphql_subscription<Query, Mutation, Subscription>(
    websocket: WebSocket,
    schema: Schema<Query, Mutation, Subscription>,
    protocol: SecWebsocketProtocol,
) where
    Query: ObjectType + Sync + Send + 'static,
    Mutation: ObjectType + Sync + Send + 'static,
    Subscription: SubscriptionType + Send + Sync + 'static,
{
    graphql_subscription_with_data(websocket, schema, protocol, |_| async {
        Ok(Default::default())
    })
    .await
}

/// GraphQL subscription handler
///
/// Specifies that a function converts the init payload to data.
pub async fn graphql_subscription_with_data<Query, Mutation, Subscription, F, R>(
    websocket: WebSocket,
    schema: Schema<Query, Mutation, Subscription>,
    protocol: SecWebsocketProtocol,
    initializer: F,
) where
    Query: ObjectType + 'static,
    Mutation: ObjectType + 'static,
    Subscription: SubscriptionType + 'static,
    F: FnOnce(Value) -> R + Send + 'static,
    R: Future<Output = Result<Data>> + Send + 'static,
{
    let (mut sink, stream) = websocket.split();
    let input = stream
        .take_while(|res| future::ready(res.is_ok()))
        .map(Result::unwrap)
        .filter(|msg| future::ready(msg.is_text() || msg.is_binary()))
        .map(Message::into_bytes);

    let mut stream =
        async_graphql::http::WebSocket::with_data(schema, input, initializer, protocol.0).map(
            |msg| match msg {
                WsMessage::Text(text) => Message::text(text),
                WsMessage::Close(code, status) => Message::close_with(code, Cow::from(status)),
            },
        );

    while let Some(item) = stream.next().await {
        let _ = sink.send(item).await;
    }
}
