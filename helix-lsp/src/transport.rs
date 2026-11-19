use crate::{
    jsonrpc,
    lsp::{self, notification::Notification as _},
    Error, LanguageServerId, Result, ServerCall,
};
use anyhow::Context;
use log::{error, info};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
    process::{ChildStderr, ChildStdin, ChildStdout},
    sync::{
        mpsc::{unbounded_channel, Sender, UnboundedReceiver, UnboundedSender},
        Mutex, Notify,
    },
};

#[derive(Debug)]
pub enum Payload {
    Request {
        chan: Sender<Result<Value>>,
        value: jsonrpc::MethodCall,
    },
    Notification(jsonrpc::Notification),
    /// A notification that the transport makes, for one too large to make on the editor's thread
    LaterNotification(LaterNotification),
    Response(jsonrpc::Output),
}

/// Makes a notification serialized.
pub struct LaterNotification(pub Box<dyn FnOnce() -> serde_json::Result<String> + Send + Sync>);

impl std::fmt::Debug for LaterNotification {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("LaterNotification")
    }
}

/// A type representing all possible values sent from the server to the client.
#[derive(Debug, PartialEq, Clone)]
enum ServerMessage {
    /// A regular JSON-RPC request output (single response).
    Output(jsonrpc::Output),
    /// A JSON-RPC request or notification.
    Call(jsonrpc::Call),
}

/// The fields of any message from a server, which tell what kind of message it is. Deserializing
/// one struct reads a message once, where trying each kind in turn would read it for each.
#[derive(Deserialize)]
struct RawMessage {
    jsonrpc: Option<jsonrpc::Version>,
    #[serde(default, deserialize_with = "present")]
    id: Option<jsonrpc::Id>,
    method: Option<String>,
    params: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    result: Option<Value>,
    error: Option<jsonrpc::Error>,
}

/// Deserializes a field that is present, even as `null`.
fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> std::result::Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

impl TryFrom<RawMessage> for ServerMessage {
    type Error = Error;

    fn try_from(raw: RawMessage) -> Result<Self> {
        let RawMessage {
            jsonrpc,
            id,
            method,
            params,
            result,
            error,
        } = raw;
        let params = match params {
            None | Some(Value::Null) => jsonrpc::Params::None,
            Some(Value::Array(params)) => jsonrpc::Params::Array(params),
            Some(Value::Object(params)) => jsonrpc::Params::Map(params),
            Some(params) => return Err(anyhow::anyhow!("invalid params: {params}").into()),
        };
        let message = match (id, method, result, error) {
            (Some(id), _, _, Some(error)) => {
                Self::Output(jsonrpc::Output::Failure(jsonrpc::Failure {
                    jsonrpc,
                    error,
                    id,
                }))
            }
            (Some(id), _, Some(result), None) => {
                Self::Output(jsonrpc::Output::Success(jsonrpc::Success {
                    jsonrpc,
                    result,
                    id,
                }))
            }
            (Some(id), Some(method), None, None) => {
                Self::Call(jsonrpc::Call::MethodCall(jsonrpc::MethodCall {
                    jsonrpc,
                    method,
                    params,
                    id,
                }))
            }
            (None, Some(method), None, None) => {
                Self::Call(jsonrpc::Call::Notification(jsonrpc::Notification {
                    jsonrpc,
                    method,
                    params,
                }))
            }
            (id, ..) => Self::Call(jsonrpc::Call::Invalid {
                id: id.unwrap_or(jsonrpc::Id::Null),
            }),
        };
        Ok(message)
    }
}

#[derive(Debug)]
pub struct Transport {
    id: LanguageServerId,
    name: String,
    pending_requests: Mutex<HashMap<jsonrpc::Id, Sender<Result<Value>>>>,
    shutdown_requested: AtomicBool,
    inject_tx: UnboundedSender<Payload>,
    /// Notified once the `exit` notification has been flushed to the server's stdin
    shutdown_flushed: Arc<Notify>,
}

impl Transport {
    #[allow(clippy::type_complexity)]
    pub fn start(
        server_stdout: BufReader<ChildStdout>,
        server_stdin: BufWriter<ChildStdin>,
        server_stderr: BufReader<ChildStderr>,
        id: LanguageServerId,
        name: String,
    ) -> (
        UnboundedReceiver<(LanguageServerId, ServerCall)>,
        UnboundedSender<Payload>,
        Arc<Notify>,
        Arc<Notify>,
    ) {
        let (client_tx, rx) = unbounded_channel();
        let (tx, client_rx) = unbounded_channel();
        let (inject_tx, inject_rx) = unbounded_channel();
        let notify = Arc::new(Notify::new());
        let shutdown_flushed = Arc::new(Notify::new());

        let transport = Self {
            id,
            name,
            pending_requests: Mutex::new(HashMap::default()),
            shutdown_requested: AtomicBool::new(false),
            inject_tx,
            shutdown_flushed: shutdown_flushed.clone(),
        };

        let transport = Arc::new(transport);

        tokio::spawn(Self::recv(
            transport.clone(),
            server_stdout,
            client_tx.clone(),
        ));
        tokio::spawn(Self::err(transport.clone(), server_stderr));
        tokio::spawn(Self::send(
            transport,
            server_stdin,
            client_tx,
            client_rx,
            inject_rx,
            notify.clone(),
        ));

        (rx, tx, notify, shutdown_flushed)
    }

    async fn recv_server_message(
        reader: &mut (impl AsyncBufRead + Unpin + Send),
        buffer: &mut String,
        content: &mut Vec<u8>,
        language_server_name: &str,
    ) -> Result<ServerMessage> {
        let mut content_length = None;
        loop {
            buffer.clear();
            if reader.read_line(buffer).await? == 0 {
                return Err(Error::StreamClosed);
            }

            // debug!("<- header {:?}", buffer);

            if buffer == "\r\n" {
                // look for an empty CRLF line
                break;
            }

            let header = buffer.trim();

            let parts = header.split_once(": ");

            match parts {
                Some(("Content-Length", value)) => {
                    content_length = Some(value.parse().context("invalid content length")?);
                }
                Some((_, _)) => {}
                None => {
                    // Workaround: Some non-conformant language servers will output logging and other garbage
                    // into the same stream as JSON-RPC messages. This can also happen from shell scripts that spawn
                    // the server. Skip such lines and log a warning.

                    // warn!("Failed to parse header: {:?}", header);
                }
            }
        }

        let content_length = content_length.context("missing content length")?;
        content.resize(content_length, 0);
        reader.read_exact(content).await?;
        let msg = std::str::from_utf8(content).context("invalid utf8 from server")?;

        info!("{language_server_name} <- {msg}");

        // NOTE: We avoid using `?` here, since it would return early on error
        // and skip clearing `content`. By returning the result directly instead,
        // we ensure `content.clear()` is always called.
        let output = sonic_rs::from_slice::<RawMessage>(content)
            .map_err(Into::into)
            .and_then(ServerMessage::try_from);

        content.clear();

        output
    }

    async fn recv_server_error(
        err: &mut (impl AsyncBufRead + Unpin + Send),
        buffer: &mut String,
        language_server_name: &str,
    ) -> Result<()> {
        buffer.truncate(0);
        if err.read_line(buffer).await? == 0 {
            return Err(Error::StreamClosed);
        };
        error!("{language_server_name} err <- {buffer:?}");

        Ok(())
    }

    async fn send_payload_to_server(
        &self,
        server_stdin: &mut BufWriter<ChildStdin>,
        payload: Payload,
    ) -> Result<()> {
        //TODO: reuse string
        let json = match payload {
            Payload::Request { chan, value } => {
                self.pending_requests
                    .lock()
                    .await
                    .insert(value.id.clone(), chan);
                serde_json::to_string(&value)?
            }
            Payload::Notification(value) => serde_json::to_string(&value)?,
            Payload::LaterNotification(LaterNotification(make)) => make()?,
            Payload::Response(error) => serde_json::to_string(&error)?,
        };
        self.send_string_to_server(server_stdin, json, &self.name)
            .await
    }

    async fn send_string_to_server(
        &self,
        server_stdin: &mut BufWriter<ChildStdin>,
        request: String,
        language_server_name: &str,
    ) -> Result<()> {
        info!("{language_server_name} -> {request}");

        // send the headers
        server_stdin
            .write_all(format!("Content-Length: {}\r\n\r\n", request.len()).as_bytes())
            .await?;

        // send the body
        server_stdin.write_all(request.as_bytes()).await?;

        server_stdin.flush().await?;

        Ok(())
    }

    async fn process_server_message(
        &self,
        client_tx: &UnboundedSender<(LanguageServerId, ServerCall)>,
        msg: ServerMessage,
        language_server_name: &str,
    ) -> Result<()> {
        match msg {
            ServerMessage::Output(output) => {
                self.process_request_response(output, language_server_name)
                    .await?
            }
            ServerMessage::Call(jsonrpc::Call::MethodCall(ref method_call))
                if self.shutdown_requested.load(Ordering::Acquire) =>
            {
                // After helix sends shutdown the application event loop is no longer
                // consuming server-to-client requests. Respond with null success so the
                // server is not left waiting for a reply before it sends the shutdown
                // response. Sending an error is intentionally avoided: servers based on
                // vscode-languageserver-node (including gopls) treat an error response to
                // client/registerCapability as fatal and abort rather than completing the
                // handshake.
                let _ = self
                    .inject_tx
                    .send(Payload::Response(jsonrpc::Output::Success(
                        jsonrpc::Success {
                            jsonrpc: Some(jsonrpc::Version::V2),
                            id: method_call.id.clone(),
                            result: serde_json::Value::Null,
                        },
                    )));
            }
            ServerMessage::Call(call) => {
                // parsed here, as large params take a while
                client_tx
                    .send((self.id, call.into()))
                    .context("failed to send a message to server")?;
            }
        };
        Ok(())
    }

    async fn process_request_response(
        &self,
        output: jsonrpc::Output,
        language_server_name: &str,
    ) -> Result<()> {
        let (id, result) = match output {
            jsonrpc::Output::Success(jsonrpc::Success { id, result, .. }) => (id, Ok(result)),
            jsonrpc::Output::Failure(jsonrpc::Failure { id, error, .. }) => {
                // requests are cancelled when nothing waits for their reply anymore
                if error.code == jsonrpc::ErrorCode::ServerError(-32800) {
                    log::debug!("{language_server_name} <- {error}");
                } else {
                    error!("{language_server_name} <- {error}");
                }
                (id, Err(error.into()))
            }
        };

        if let Some(tx) = self.pending_requests.lock().await.remove(&id) {
            match tx.send(result).await {
                Ok(_) => (),
                Err(_) => log::debug!(
                    "Tried sending response into a closed channel (id={:?}), likely a fire-and-forget shutdown",
                    id
                ),
            };
        } else {
            log::error!(
                "Discarding Language Server response without a request (id={:?}) {:?}",
                id,
                result
            );
        }

        Ok(())
    }

    async fn recv(
        transport: Arc<Self>,
        mut server_stdout: BufReader<ChildStdout>,
        client_tx: UnboundedSender<(LanguageServerId, ServerCall)>,
    ) {
        let mut recv_buffer = String::new();
        let mut content_buffer = Vec::new();
        loop {
            match Self::recv_server_message(
                &mut server_stdout,
                &mut recv_buffer,
                &mut content_buffer,
                &transport.name,
            )
            .await
            {
                Ok(msg) => {
                    match transport
                        .process_server_message(&client_tx, msg, &transport.name)
                        .await
                    {
                        Ok(_) => {}
                        Err(err) => {
                            error!("{} err: <- {err:?}", transport.name);
                            break;
                        }
                    };
                }
                Err(err) => {
                    if !matches!(err, Error::StreamClosed) {
                        error!(
                            "Exiting {} after unexpected error: {err:?}",
                            &transport.name
                        );
                    }

                    // Close any outstanding requests.
                    for (id, tx) in transport.pending_requests.lock().await.drain() {
                        match tx.send(Err(Error::StreamClosed)).await {
                            Ok(_) => (),
                            Err(_) => {
                                error!("Could not close request on a closed channel (id={:?})", id)
                            }
                        }
                    }

                    // Hack: inject a terminated notification so we trigger code that needs to happen after exit
                    let notification =
                        ServerMessage::Call(jsonrpc::Call::Notification(jsonrpc::Notification {
                            jsonrpc: None,
                            method: lsp::notification::Exit::METHOD.to_string(),
                            params: jsonrpc::Params::None,
                        }));
                    match transport
                        .process_server_message(&client_tx, notification, &transport.name)
                        .await
                    {
                        Ok(_) => {}
                        Err(err) => {
                            error!("err: <- {:?}", err);
                        }
                    }
                    break;
                }
            }
        }
    }

    async fn err(transport: Arc<Self>, mut server_stderr: BufReader<ChildStderr>) {
        let mut recv_buffer = String::new();
        loop {
            match Self::recv_server_error(&mut server_stderr, &mut recv_buffer, &transport.name)
                .await
            {
                Ok(_) => {}
                Err(err) => {
                    error!("{} err: <- {err:?}", transport.name);
                    break;
                }
            }
        }
    }

    async fn send(
        transport: Arc<Self>,
        mut server_stdin: BufWriter<ChildStdin>,
        client_tx: UnboundedSender<(LanguageServerId, ServerCall)>,
        mut client_rx: UnboundedReceiver<Payload>,
        mut inject_rx: UnboundedReceiver<Payload>,
        initialize_notify: Arc<Notify>,
    ) {
        let mut pending_messages: Vec<Payload> = Vec::new();
        let mut is_pending = true;

        // Pin outside the loop to avoid cancellation-safety issue:
        // recreating `notified()` inside `select!` can lose the permit.
        let notified = initialize_notify.notified();
        tokio::pin!(notified);

        // Determine if a message is allowed to be sent early
        fn is_initialize(payload: &Payload) -> bool {
            use lsp::{
                notification::Initialized,
                request::{Initialize, Request},
            };
            match payload {
                Payload::Request {
                    value: jsonrpc::MethodCall { method, .. },
                    ..
                } if method == Initialize::METHOD => true,
                Payload::Notification(jsonrpc::Notification { method, .. })
                    if method == Initialized::METHOD =>
                {
                    true
                }
                _ => false,
            }
        }

        fn is_shutdown(payload: &Payload) -> bool {
            use lsp::request::{Request, Shutdown};
            matches!(payload, Payload::Request { value: jsonrpc::MethodCall { method, .. }, .. } if method == Shutdown::METHOD)
        }

        fn is_exit(payload: &Payload) -> bool {
            use lsp::notification::{Exit, Notification};
            matches!(payload, Payload::Notification(jsonrpc::Notification { method, .. }) if method == Exit::METHOD)
        }

        // TODO: events that use capabilities need to do the right thing

        loop {
            tokio::select! {
                biased;
                _ = &mut notified, if is_pending => {
                    // server successfully initialized
                    is_pending = false;

                    // Hack: inject an initialized notification so we trigger code that needs to happen after init
                    let notification = ServerMessage::Call(jsonrpc::Call::Notification(jsonrpc::Notification {
                        jsonrpc: None,

                        method: lsp::notification::Initialized::METHOD.to_string(),
                        params: jsonrpc::Params::None,
                    }));
                    let language_server_name = &transport.name;
                    match transport.process_server_message(&client_tx, notification, language_server_name).await {
                        Ok(_) => {}
                        Err(err) => {
                            error!("{language_server_name} err: <- {err:?}");
                        }
                    }

                    // drain the pending queue and send payloads to server
                    for msg in pending_messages.drain(..) {
                        log::info!("Draining pending message {:?}", msg);
                        match transport.send_payload_to_server(&mut server_stdin, msg).await {
                            Ok(_) => {}
                            Err(err) => {
                                error!("{language_server_name} err: <- {err:?}");
                            }
                        }
                    }
                }
                msg = client_rx.recv() => {
                    if let Some(msg) = msg {
                        if is_pending && is_shutdown(&msg) {
                            log::info!("Language server not initialized, shutting down");
                            break;
                        } else if is_pending && !is_initialize(&msg) {
                            // ignore notifications
                            if let Payload::Notification(_) | Payload::LaterNotification(_) = msg {
                                continue;
                            }

                            log::info!("Language server not initialized, delaying request");
                            pending_messages.push(msg);
                        } else {
                            let is_shutdown_msg = is_shutdown(&msg);
                            let is_exit_msg = is_exit(&msg);
                            // Set the flag *before* flushing to stdin so that the recv task
                            // cannot observe an unanswered server request in the window between
                            // the kernel delivering the bytes to the server and this store.
                            if is_shutdown_msg {
                                transport
                                    .shutdown_requested
                                    .store(true, Ordering::Release);
                            }
                            match transport.send_payload_to_server(&mut server_stdin, msg).await {
                                Ok(_) => {
                                    // `exit` is the last thing a shutting-down client sends;
                                    // signal that it has reached the server's stdin.
                                    if is_exit_msg {
                                        transport.shutdown_flushed.notify_one();
                                    }
                                }
                                Err(err) => {
                                    error!("{} err: <- {err:?}", transport.name);
                                }
                            }
                        }
                    } else {
                        // channel closed
                        break;
                    }
                }
                msg = inject_rx.recv() => {
                    if let Some(msg) = msg {
                        match transport.send_payload_to_server(&mut server_stdin, msg).await {
                            Ok(_) => {}
                            Err(err) => {
                                error!("{} inject err: <- {err:?}", transport.name);
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// The messages the untagged enums of `jsonrpc` read, as the server message they read them as.
    #[test]
    fn messages_are_read_as_their_kind() {
        let message = |json: &str| {
            ServerMessage::try_from(sonic_rs::from_str::<RawMessage>(json).unwrap()).unwrap()
        };
        let output = |json| match message(json) {
            ServerMessage::Output(output) => output,
            other => panic!("{json}: {other:?}"),
        };
        let call = |json| match message(json) {
            ServerMessage::Call(call) => call,
            other => panic!("{json}: {other:?}"),
        };
        let untagged_output = |json| serde_json::from_str::<jsonrpc::Output>(json).unwrap();
        let untagged_call = |json| serde_json::from_str::<jsonrpc::Call>(json).unwrap();

        for json in [
            r#"{"jsonrpc":"2.0","id":1,"result":{"a":[1,2]}}"#,
            r#"{"jsonrpc":"2.0","id":"x","result":null}"#,
            r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32800,"message":"cancelled"}}"#,
            r#"{"id":3,"result":1,"error":{"code":-32601,"message":"both"}}"#,
        ] {
            assert_eq!(output(json), untagged_output(json), "{json}");
        }
        for json in [
            r#"{"jsonrpc":"2.0","id":4,"method":"workspace/configuration","params":{"items":[]}}"#,
            r#"{"jsonrpc":"2.0","id":null,"method":"m"}"#,
            r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///a","diagnostics":[]}}"#,
            r#"{"jsonrpc":"2.0","method":"exit"}"#,
            r#"{"method":"m","params":[1,null]}"#,
            r#"{"id":5}"#,
        ] {
            assert_eq!(call(json), untagged_call(json), "{json}");
        }
    }
}
