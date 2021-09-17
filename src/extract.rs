use std::collections::HashMap;

use async_graphql::{http::MultipartOptions, ParseRequestError};

use viz_core::{http, types::Multipart, Context, Error, Extract, Result};
use viz_utils::{
    futures::{future::BoxFuture, TryStreamExt},
    serde::json,
};

/// Extractor for GraphQL request.
pub struct GraphQLRequest(pub async_graphql::Request);

impl GraphQLRequest {
    /// Unwraps the value to `async_graphql::Request`.
    #[must_use]
    pub fn into_inner(self) -> async_graphql::Request {
        self.0
    }
}

/// Rejection response types.
pub mod rejection {
    use async_graphql::ParseRequestError;
    use viz_core::{http, Response};

    /// Rejection used for [`GraphQLRequest`](GraphQLRequest).
    pub struct GraphQLRejection(pub ParseRequestError);

    impl From<GraphQLRejection> for Response {
        fn from(gr: GraphQLRejection) -> Self {
            match gr.0 {
                ParseRequestError::PayloadTooLarge => http::StatusCode::PAYLOAD_TOO_LARGE.into(),
                bad_request => (http::StatusCode::BAD_REQUEST, format!("{:?}", bad_request)).into(),
            }
        }
    }

    impl From<ParseRequestError> for GraphQLRejection {
        fn from(err: ParseRequestError) -> Self {
            GraphQLRejection(err)
        }
    }
}

impl Extract for GraphQLRequest {
    type Error = rejection::GraphQLRejection;

    fn extract(cx: &mut Context) -> BoxFuture<'_, Result<Self, Self::Error>> {
        Box::pin(async move {
            Ok(GraphQLRequest(
                GraphQLBatchRequest::extract(cx).await?.0.into_single()?,
            ))
        })
    }
}

/// Extractor for GraphQL batch request.
pub struct GraphQLBatchRequest(pub async_graphql::BatchRequest);

impl GraphQLBatchRequest {
    /// Unwraps the value to `async_graphql::BatchRequest`.
    #[must_use]
    pub fn into_inner(self) -> async_graphql::BatchRequest {
        self.0
    }
}

impl Extract for GraphQLBatchRequest {
    type Error = rejection::GraphQLRejection;

    fn extract(cx: &mut Context) -> BoxFuture<'_, Result<Self, Self::Error>> {
        Box::pin(async move {
            if http::Method::GET == cx.method() {
                Ok(Self(async_graphql::BatchRequest::Single(
                    cx.query()
                        .map_err(|e| ParseRequestError::InvalidRequest(Box::from(e)))?,
                )))
            } else {
                if let Ok(multipart) = cx.multipart() {
                    if let Ok(mut state) = multipart.state().lock() {
                        let opts = MultipartOptions::default();
                        let mut limits = state.limits_mut();
                        limits.file_size = opts.max_file_size;
                        limits.files = opts.max_num_files;
                    }

                    Ok(Self(receive_batch_multipart(multipart).await.map_err(
                        |e| ParseRequestError::InvalidRequest(Box::from(e)),
                    )?))
                } else {
                    Ok(Self(cx.json().await.map_err(|e| {
                        ParseRequestError::InvalidRequest(Box::from(e))
                    })?))
                }
            }
        })
    }
}

async fn receive_batch_multipart(mut multipart: Multipart) -> Result<async_graphql::BatchRequest> {
    let mut request = None;
    let mut map = None;
    let mut files = Vec::new();

    while let Some(mut field) = multipart.try_next().await? {
        // in multipart, each field / file can actually have a own Content-Type.
        // We use this to determine the encoding of the graphql query
        let content_type = field
            .content_type
            .to_owned()
            .unwrap_or(mime::APPLICATION_JSON);

        let name = field.name.clone();

        match name.as_str() {
            "operations" => {
                let body = field.bytes().await?;
                request = Some(json::from_slice(&body)?)
            }
            "map" => {
                let map_bytes = field.bytes().await?;

                match (content_type.type_(), content_type.subtype()) {
                    // cbor is in application/octet-stream.
                    // TODO: wait for mime to add application/cbor and match against that too
                    // Note: we actually differ here from the inoffical spec for this:
                    // (https://github.com/jaydenseric/graphql-multipart-request-spec#multipart-form-field-structure)
                    // It says: "map: A JSON encoded map of where files occurred in the operations.
                    // For each file, the key is the file multipart form field name and the value is an array of operations paths."
                    // However, I think, that since we accept CBOR as operation, which is valid, we should also accept it
                    // as the mapping for the files.
                    #[cfg(feature = "cbor")]
                    (mime::OCTET_STREAM, _) | (mime::APPLICATION, mime::OCTET_STREAM) => {
                        map = Some(
                            serde_cbor::from_slice::<HashMap<String, Vec<String>>>(&map_bytes)
                                .map_err(|e| ParseRequestError::InvalidFilesMap(Box::new(e)))?,
                        );
                    }
                    // default to json
                    _ => {
                        map = Some(
                            json::from_slice::<HashMap<String, Vec<String>>>(&map_bytes)
                                .map_err(|e| ParseRequestError::InvalidFilesMap(Box::new(e)))?,
                        );
                    }
                }
            }
            _ => {
                if !name.is_empty() {
                    if let Some(filename) = field.filename.to_owned() {
                        let mut file = tempfile::tempfile().map_err(ParseRequestError::Io)?;
                        field.copy_to_file(&mut file).await?;
                        files.push((name, filename, Some(content_type.to_string()), file));
                    }
                }
            }
        }
    }

    let mut request = request.ok_or(ParseRequestError::MissingOperatorsPart)?;
    let map = map.as_mut().ok_or(ParseRequestError::MissingMapPart)?;

    for (name, filename, content_type, content) in files {
        if let Some(var_paths) = map.remove(&name) {
            let upload = async_graphql::UploadValue {
                filename,
                content_type,
                content,
            };

            for var_path in var_paths {
                match &mut request {
                    async_graphql::BatchRequest::Single(request) => {
                        request.set_upload(&var_path, upload.try_clone()?);
                    }
                    async_graphql::BatchRequest::Batch(requests) => {
                        let mut s = var_path.splitn(2, '.');
                        let idx = s.next().and_then(|idx| idx.parse::<usize>().ok());
                        let path = s.next();

                        if let (Some(idx), Some(path)) = (idx, path) {
                            if let Some(request) = requests.get_mut(idx) {
                                request.set_upload(path, upload.try_clone()?);
                            }
                        }
                    }
                }
            }
        }
    }

    if !map.is_empty() {
        return Err(Error::from(async_graphql::ParseRequestError::MissingFiles));
    }

    Ok(request)
}
