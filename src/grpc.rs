//! gRPC echo service.
//!
//! Runs a Tonic-based `EchoService` on its own port (default 50051,
//! `RUSTYBIN_GRPC_PORT`) covering unary, server-streaming, client-streaming,
//! and bidirectional-streaming calls. This gives API gateway gRPC proxying
//! (grpc-proxy / grpc-web / grpc-transcoding features) a real upstream to target.

use std::pin::Pin;

use tokio_stream::{Stream, StreamExt};
use tonic::{transport::Server, Request, Response, Status, Streaming};

pub mod pb {
    tonic::include_proto!("rustybin.echo.v1");
}

use pb::echo_service_server::{EchoService, EchoServiceServer};
use pb::{EchoRequest, EchoResponse};

#[derive(Clone)]
pub struct EchoSvc {
    instance_id: String,
}

/// Collect gRPC request metadata (headers) into the response map, lower-cased,
/// skipping binary (`-bin`) keys which aren't valid UTF-8.
fn reflect_metadata<T>(req: &Request<T>) -> std::collections::HashMap<String, String> {
    req.metadata()
        .iter()
        .filter_map(|kv| match kv {
            tonic::metadata::KeyAndValueRef::Ascii(k, v) => {
                Some((k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
            }
            tonic::metadata::KeyAndValueRef::Binary(_, _) => None,
        })
        .collect()
}

type ResponseStream = Pin<Box<dyn Stream<Item = Result<EchoResponse, Status>> + Send>>;

#[tonic::async_trait]
impl EchoService for EchoSvc {
    async fn echo(&self, request: Request<EchoRequest>) -> Result<Response<EchoResponse>, Status> {
        let metadata = reflect_metadata(&request);
        let msg = request.into_inner().message;
        Ok(Response::new(EchoResponse {
            message: msg,
            metadata,
            instance_id: self.instance_id.clone(),
            index: 0,
        }))
    }

    type ServerStreamStream = ResponseStream;

    async fn server_stream(
        &self,
        request: Request<EchoRequest>,
    ) -> Result<Response<Self::ServerStreamStream>, Status> {
        let metadata = reflect_metadata(&request);
        let instance_id = self.instance_id.clone();
        let inner = request.into_inner();
        let count = inner.count.clamp(1, 100);
        let message = inner.message;

        let stream = async_stream::stream! {
            for index in 0..count {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                yield Ok(EchoResponse {
                    message: message.clone(),
                    metadata: metadata.clone(),
                    instance_id: instance_id.clone(),
                    index,
                });
            }
        };

        Ok(Response::new(Box::pin(stream) as ResponseStream))
    }

    async fn client_stream(
        &self,
        request: Request<Streaming<EchoRequest>>,
    ) -> Result<Response<EchoResponse>, Status> {
        let metadata = reflect_metadata(&request);
        let mut stream = request.into_inner();
        let mut messages = Vec::new();
        while let Some(req) = stream.next().await {
            messages.push(req?.message);
        }
        let count = messages.len() as u32;
        Ok(Response::new(EchoResponse {
            message: messages.join(" "),
            metadata,
            instance_id: self.instance_id.clone(),
            index: count,
        }))
    }

    type BidiStreamStream = ResponseStream;

    async fn bidi_stream(
        &self,
        request: Request<Streaming<EchoRequest>>,
    ) -> Result<Response<Self::BidiStreamStream>, Status> {
        let metadata = reflect_metadata(&request);
        let instance_id = self.instance_id.clone();
        let mut stream = request.into_inner();

        let out = async_stream::stream! {
            let mut index = 0u32;
            while let Some(req) = stream.next().await {
                match req {
                    Ok(r) => {
                        yield Ok(EchoResponse {
                            message: r.message,
                            metadata: metadata.clone(),
                            instance_id: instance_id.clone(),
                            index,
                        });
                        index += 1;
                    }
                    Err(e) => {
                        yield Err(e);
                        break;
                    }
                }
            }
        };

        Ok(Response::new(Box::pin(out) as ResponseStream))
    }
}

/// Serve the gRPC EchoService on an already-bound listener until `shutdown`
/// resolves.
pub async fn serve<F>(
    listener: tokio::net::TcpListener,
    instance_id: String,
    shutdown: F,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    F: std::future::Future<Output = ()> + Send,
{
    let svc = EchoSvc { instance_id };
    let addr = listener.local_addr()?;
    tracing::info!("gRPC listening on {addr} (EchoService)");
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    Server::builder()
        .add_service(EchoServiceServer::new(svc))
        .serve_with_incoming_shutdown(incoming, shutdown)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::pb::echo_service_client::EchoServiceClient;
    use super::pb::{EchoRequest, EchoResponse};
    use super::*;
    use tokio_stream::StreamExt;

    async fn spawn() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        tokio::spawn(async move {
            Server::builder()
                .add_service(EchoServiceServer::new(EchoSvc {
                    instance_id: "test-instance".to_string(),
                }))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn unary_echo() {
        let url = spawn().await;
        let mut client = EchoServiceClient::connect(url).await.unwrap();
        let mut req = Request::new(EchoRequest {
            message: "ping".to_string(),
            count: 0,
        });
        req.metadata_mut().insert("x-demo", "abc".parse().unwrap());
        let resp = client.echo(req).await.unwrap().into_inner();
        assert_eq!(resp.message, "ping");
        assert_eq!(resp.instance_id, "test-instance");
        assert_eq!(resp.metadata.get("x-demo").map(String::as_str), Some("abc"));
    }

    #[tokio::test]
    async fn server_streaming() {
        let url = spawn().await;
        let mut client = EchoServiceClient::connect(url).await.unwrap();
        let resp = client
            .server_stream(EchoRequest {
                message: "tick".to_string(),
                count: 4,
            })
            .await
            .unwrap();
        let mut stream = resp.into_inner();
        let mut seen = 0u32;
        while let Some(item) = stream.next().await {
            let item = item.unwrap();
            assert_eq!(item.message, "tick");
            assert_eq!(item.index, seen);
            seen += 1;
        }
        assert_eq!(seen, 4);
    }

    #[tokio::test]
    async fn client_streaming() {
        let url = spawn().await;
        let mut client = EchoServiceClient::connect(url).await.unwrap();
        let outbound = tokio_stream::iter(vec![
            EchoRequest {
                message: "a".to_string(),
                count: 0,
            },
            EchoRequest {
                message: "b".to_string(),
                count: 0,
            },
            EchoRequest {
                message: "c".to_string(),
                count: 0,
            },
        ]);
        let resp: EchoResponse = client.client_stream(outbound).await.unwrap().into_inner();
        assert_eq!(resp.message, "a b c");
        assert_eq!(resp.index, 3);
    }

    #[tokio::test]
    async fn bidi_streaming() {
        let url = spawn().await;
        let mut client = EchoServiceClient::connect(url).await.unwrap();
        let outbound = tokio_stream::iter(vec![
            EchoRequest {
                message: "one".to_string(),
                count: 0,
            },
            EchoRequest {
                message: "two".to_string(),
                count: 0,
            },
        ]);
        let resp = client.bidi_stream(outbound).await.unwrap();
        let mut stream = resp.into_inner();
        let mut msgs = Vec::new();
        while let Some(item) = stream.next().await {
            msgs.push(item.unwrap().message);
        }
        assert_eq!(msgs, vec!["one".to_string(), "two".to_string()]);
    }
}
