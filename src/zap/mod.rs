use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use api::rest::SearchRequestInternal;
use collection::operations::point_ops::PointsSelector;
use collection::operations::shard_selector_internal::ShardSelectorInternal;
use collection::operations::types::PointRequestInternal;
use collection::operations::verification::new_unchecked_verification_pass;
use common::counter::hardware_accumulator::HwMeasurementAcc;
use segment::types::WithPayloadInterface;
use storage::content_manager::collection_meta_ops::{
    CollectionMetaOperations, CreateCollectionOperation,
};
use storage::dispatcher::Dispatcher;
use storage::rbac::{Access, Auth};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Handle;
use tokio::signal;

use crate::common::collections::do_list_collections;
use crate::common::query::{do_core_search_points, do_get_points};
use crate::common::strict_mode::StrictModeCheckedTocProvider;
use crate::common::update::{
    InternalUpdateParams, UpdateParams, do_delete_points, do_upsert_points,
};
use crate::settings::Settings;

// Opcodes
const OP_UPSERT: u16 = 0x01;
const OP_SEARCH: u16 = 0x02;
const OP_DELETE: u16 = 0x03;
const OP_GET: u16 = 0x04;
const OP_CREATE_COLLECTION: u16 = 0x05;
const OP_LIST_COLLECTIONS: u16 = 0x06;
const OP_HEALTH: u16 = 0x07;

// Status codes in response
const STATUS_OK: u16 = 0x00;
const STATUS_ERR: u16 = 0xFF;

/// Read a ZAP frame: 2-byte opcode (big-endian) + 4-byte length (big-endian) + payload.
async fn read_frame(stream: &mut TcpStream) -> io::Result<(u16, Vec<u8>)> {
    let opcode = stream.read_u16().await?;
    let len = stream.read_u32().await?;
    let mut payload = vec![0u8; len as usize];
    if len > 0 {
        stream.read_exact(&mut payload).await?;
    }
    Ok((opcode, payload))
}

/// Write a ZAP response frame: 2-byte status + 4-byte length + payload.
async fn write_frame(stream: &mut TcpStream, status: u16, payload: &[u8]) -> io::Result<()> {
    stream.write_u16(status).await?;
    stream.write_u32(payload.len() as u32).await?;
    if !payload.is_empty() {
        stream.write_all(payload).await?;
    }
    stream.flush().await?;
    Ok(())
}

fn ok_response(data: &serde_json::Value) -> (u16, Vec<u8>) {
    (STATUS_OK, serde_json::to_vec(data).unwrap_or_default())
}

fn err_response(msg: &str) -> (u16, Vec<u8>) {
    let body = serde_json::json!({"error": msg});
    (STATUS_ERR, serde_json::to_vec(&body).unwrap_or_default())
}

fn full_auth() -> Auth {
    Auth::new_internal(Access::full("ZAP transport"))
}

async fn handle_connection(mut stream: TcpStream, dispatcher: Arc<Dispatcher>) {
    loop {
        let (opcode, payload) = match read_frame(&mut stream).await {
            Ok(frame) => frame,
            Err(e) => {
                if e.kind() != io::ErrorKind::UnexpectedEof {
                    log::debug!("ZAP connection read error: {e}");
                }
                return;
            }
        };

        let (status, response_payload) = dispatch(opcode, &payload, &dispatcher).await;

        if let Err(e) = write_frame(&mut stream, status, &response_payload).await {
            log::debug!("ZAP connection write error: {e}");
            return;
        }
    }
}

async fn dispatch(opcode: u16, payload: &[u8], dispatcher: &Dispatcher) -> (u16, Vec<u8>) {
    match opcode {
        OP_HEALTH => handle_health(),
        OP_LIST_COLLECTIONS => handle_list_collections(dispatcher).await,
        OP_CREATE_COLLECTION => handle_create_collection(payload, dispatcher).await,
        OP_UPSERT => handle_upsert(payload, dispatcher).await,
        OP_SEARCH => handle_search(payload, dispatcher).await,
        OP_DELETE => handle_delete(payload, dispatcher).await,
        OP_GET => handle_get(payload, dispatcher).await,
        _ => err_response(&format!("unknown opcode: 0x{opcode:02X}")),
    }
}

fn handle_health() -> (u16, Vec<u8>) {
    ok_response(&serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn handle_list_collections(dispatcher: &Dispatcher) -> (u16, Vec<u8>) {
    let auth = full_auth();
    let pass = new_unchecked_verification_pass();
    let toc = dispatcher.toc(&auth, &pass);
    match do_list_collections(toc, &auth).await {
        Ok(resp) => ok_response(&serde_json::to_value(&resp).unwrap_or_default()),
        Err(e) => err_response(&e.to_string()),
    }
}

/// Expects JSON: {"name": "...", "params": { ... CreateCollection ... }}
async fn handle_create_collection(
    payload: &[u8],
    dispatcher: &Dispatcher,
) -> (u16, Vec<u8>) {
    #[derive(serde::Deserialize)]
    struct Req {
        name: String,
        #[serde(flatten)]
        params: storage::content_manager::collection_meta_ops::CreateCollection,
    }
    let req: Req = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return err_response(&format!("invalid payload: {e}")),
    };
    let op = match CreateCollectionOperation::new(req.name, req.params) {
        Ok(op) => op,
        Err(e) => return err_response(&format!("invalid collection params: {e}")),
    };
    let auth = full_auth();
    match dispatcher
        .submit_collection_meta_op(CollectionMetaOperations::CreateCollection(op), auth, None)
        .await
    {
        Ok(result) => ok_response(
            &serde_json::json!({"result": result}),
        ),
        Err(e) => err_response(&e.to_string()),
    }
}

/// Expects JSON: {"collection": "name", "points": [ ... PointInsertOperations ... ]}
async fn handle_upsert(payload: &[u8], dispatcher: &Dispatcher) -> (u16, Vec<u8>) {
    #[derive(serde::Deserialize)]
    struct Req {
        collection: String,
        #[serde(flatten)]
        operation: api::rest::schema::PointInsertOperations,
    }
    let req: Req = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return err_response(&format!("invalid payload: {e}")),
    };

    let auth = full_auth();
    let params = UpdateParams {
        wait: true,
        ordering: Default::default(),
        timeout: None,
    };
    let hw = HwMeasurementAcc::disposable();
    let inference_params = crate::common::inference::params::InferenceParams::new(
        crate::common::inference::api_keys::InferenceApiKeys::new(None),
        None,
    );

    match do_upsert_points(
        StrictModeCheckedTocProvider::new(dispatcher),
        req.collection,
        req.operation,
        InternalUpdateParams::default(),
        params,
        auth,
        inference_params,
        hw,
    )
    .await
    {
        Ok((result, _usage)) => {
            ok_response(&serde_json::to_value(&result).unwrap_or_default())
        }
        Err(e) => err_response(&e.to_string()),
    }
}

/// Expects JSON: {"collection": "name", "vector": [...], "limit": N, ...}
/// Uses the same SearchRequestInternal as the REST API, converted to CoreSearchRequest.
async fn handle_search(payload: &[u8], dispatcher: &Dispatcher) -> (u16, Vec<u8>) {
    #[derive(serde::Deserialize)]
    struct Req {
        collection: String,
        #[serde(flatten)]
        search_request: SearchRequestInternal,
    }
    let req: Req = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return err_response(&format!("invalid payload: {e}")),
    };

    let auth = full_auth();
    let pass = new_unchecked_verification_pass();
    let toc = dispatcher.toc(&auth, &pass);
    let hw = HwMeasurementAcc::disposable();

    // Convert REST-level request to internal CoreSearchRequest (same path as REST handler)
    let core_request = req.search_request.into();

    let result = do_core_search_points(
        toc,
        &req.collection,
        core_request,
        None,
        ShardSelectorInternal::All,
        auth,
        None,
        hw,
    )
    .await
    .map(|scored_points| {
        scored_points
            .into_iter()
            .map(api::rest::ScoredPoint::from)
            .collect::<Vec<_>>()
    });
    match result {
        Ok(results) => ok_response(&serde_json::to_value(&results).unwrap_or_default()),
        Err(e) => err_response(&e.to_string()),
    }
}

/// Expects JSON: {"collection": "name", "points": [...ids...]} or {"collection": "name", "filter": {...}}
async fn handle_delete(payload: &[u8], dispatcher: &Dispatcher) -> (u16, Vec<u8>) {
    #[derive(serde::Deserialize)]
    struct Req {
        collection: String,
        #[serde(flatten)]
        selector: PointsSelector,
    }
    let req: Req = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return err_response(&format!("invalid payload: {e}")),
    };

    let auth = full_auth();
    let params = UpdateParams {
        wait: true,
        ordering: Default::default(),
        timeout: None,
    };
    let hw = HwMeasurementAcc::disposable();

    match do_delete_points(
        StrictModeCheckedTocProvider::new(dispatcher),
        req.collection,
        req.selector,
        InternalUpdateParams::default(),
        params,
        auth,
        hw,
    )
    .await
    {
        Ok(result) => ok_response(&serde_json::to_value(&result).unwrap_or_default()),
        Err(e) => err_response(&e.to_string()),
    }
}

/// Expects JSON: {"collection": "name", "ids": [...]}
async fn handle_get(payload: &[u8], dispatcher: &Dispatcher) -> (u16, Vec<u8>) {
    #[derive(serde::Deserialize)]
    struct Req {
        collection: String,
        ids: Vec<segment::types::PointIdType>,
        #[serde(default)]
        with_payload: Option<bool>,
        #[serde(default)]
        with_vector: Option<bool>,
    }
    let req: Req = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return err_response(&format!("invalid payload: {e}")),
    };

    let auth = full_auth();
    let pass = new_unchecked_verification_pass();
    let toc = dispatcher.toc(&auth, &pass);
    let hw = HwMeasurementAcc::disposable();

    let request = PointRequestInternal {
        ids: req.ids,
        with_payload: Some(WithPayloadInterface::Bool(
            req.with_payload.unwrap_or(true),
        )),
        with_vector: req.with_vector.unwrap_or(false).into(),
    };

    let result = do_get_points(
        toc,
        &req.collection,
        request,
        None,
        None,
        ShardSelectorInternal::All,
        auth,
        hw,
    )
    .await
    .map(|records| {
        records
            .into_iter()
            .map(api::rest::Record::from)
            .collect::<Vec<_>>()
    });
    match result {
        Ok(records) => ok_response(&serde_json::to_value(&records).unwrap_or_default()),
        Err(e) => err_response(&e.to_string()),
    }
}

#[cfg(not(unix))]
async fn wait_stop_signal() {
    signal::ctrl_c().await.unwrap();
    log::debug!("Stopping ZAP service on SIGINT");
}

#[cfg(unix)]
async fn wait_stop_signal() {
    let mut term = signal::unix::signal(signal::unix::SignalKind::terminate()).unwrap();
    let mut inrt = signal::unix::signal(signal::unix::SignalKind::interrupt()).unwrap();
    tokio::select! {
        _ = term.recv() => log::debug!("Stopping ZAP service on SIGTERM"),
        _ = inrt.recv() => log::debug!("Stopping ZAP service on SIGINT"),
    }
}

pub fn init(
    dispatcher: Arc<Dispatcher>,
    settings: Settings,
    zap_port: u16,
    runtime: Handle,
) -> io::Result<()> {
    runtime.block_on(async {
        let addr = SocketAddr::from((
            settings.service.host.parse::<IpAddr>().unwrap(),
            zap_port,
        ));
        let listener = TcpListener::bind(addr).await?;
        log::info!("ZAP transport listening on {zap_port}");

        let shutdown = wait_stop_signal();
        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, peer)) => {
                            log::debug!("ZAP connection from {peer}");
                            let dispatcher = dispatcher.clone();
                            tokio::spawn(async move {
                                handle_connection(stream, dispatcher).await;
                            });
                        }
                        Err(e) => {
                            log::error!("ZAP accept error: {e}");
                        }
                    }
                }
                _ = &mut shutdown => {
                    log::info!("ZAP transport shutting down");
                    break;
                }
            }
        }

        Ok(())
    })
}
