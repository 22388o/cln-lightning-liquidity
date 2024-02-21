use std::{collections::HashMap, path::Path, sync::Arc};

mod client;
mod constants;

use client::lsps1_client::lsps1_client;
use cln_plugin::{Builder, Error, Plugin};
use cln_rpc::ClnRpc;
use constants::{CreateOrderJsonRpcResponse, GetInfoJsonRpcResponse, MESSAGE_TYPE};

use serde_json::json;
use tokio::{
    io::{stdin, stdout},
    sync::Mutex,
};

use crate::{
    client::validate_and_pay::Lsps1ValidateAndPay,
    constants::{LSPS1_CREATE_ORDER_METHOD, LSPS1_GET_ORDER_METHOD},
};

struct PluginState {
    data: Mutex<HashMap<String, String>>,
    method: Mutex<HashMap<String, String>>,
}

impl PluginState {
    async fn new() -> Result<Self, Error> {
        Ok(Self {
            data: Mutex::new(HashMap::new()),
            method: Mutex::new(HashMap::new()),
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let plugin_state = Arc::new(PluginState::new().await?);

    if let Some(plugin) = Builder::new(stdin(), stdout())
        .dynamic()
        .rpcmethod(
            "buy-inbound-channel",
            "Buy an inbound channel from other peers",
            lsps1_client,
        )
        .hook("custommsg", subscribe_to_custom_message)
        .start(plugin_state)
        .await?
    {
        let plug_res = plugin.join().await;

        plug_res
    } else {
        Ok(())
    }
}

async fn subscribe_to_custom_message(
    p: Plugin<Arc<PluginState>>,
    v: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let state_ref = p.state().clone();

    // Now, you can lock the mutex asynchronously
    let data = state_ref.data.lock().await;
    let method = state_ref.method.lock().await;

    // Attempt to extract "payload"
    let payload_hex = match v.get("payload").and_then(|v| v.as_str()) {
        Some(payload_hex) => payload_hex,
        None => {
            log::warn!("No payload found in custom message");
            return Ok(json!({ "result": "continue" }));
        }
    };

    let bytes = match hex::decode(payload_hex) {
        Ok(bytes) => bytes,
        _ => {
            return Ok(json!({ "result": "continue" }));
        }
    };

    // Ensure there are at least 2 bytes for the message type
    if bytes.len() < 2 {
        return Ok(json!({ "result": "continue" }));
    }

    // Extract the message type from the first 2 bytes
    let message_type = u16::from_be_bytes([bytes[0], bytes[1]]);

    if message_type != MESSAGE_TYPE {
        return Ok(json!({ "result": "continue" }));
    }

    let conf = p.configuration();
    let socket_path = Path::new(&conf.lightning_dir).join(&conf.rpc_file);
    let client = ClnRpc::new(socket_path).await?;

    // Extract the JSON payload starting from the 3rd byte
    let json_bytes = &bytes[2..];

    // Get info method response
    match serde_json::from_slice::<GetInfoJsonRpcResponse>(json_bytes) {
        Ok(json_payload) => {
            log::info!("GetInfo Decoded JSON payload: {:?}", json_payload)
        }
        Err(e) => {
            log::warn!("GetInfo Failed to decode JSON payload: {}", e)
        }
    };

    // Get order response method
    // Get order and create order have the same response from server
    match serde_json::from_slice::<CreateOrderJsonRpcResponse>(json_bytes) {
        Ok(json_payload) => {
            if method.get(&"method".to_string()) == Some(&LSPS1_GET_ORDER_METHOD.to_string()) {
                log::info!("GetOrder Decoded JSON payload: {:?}", json_payload);
            }
        }
        Err(e) => {
            log::warn!("CreateOrder Failed to decode JSON payload: {}", e);
        }
    };

    // Create order response method
    match serde_json::from_slice::<CreateOrderJsonRpcResponse>(json_bytes) {
        Ok(json_payload) => {
            if method.get(&"method".to_string()) != Some(&LSPS1_CREATE_ORDER_METHOD.to_string()) {
                return Ok(json!({ "result": "continue" }));
            }

            log::info!("CreateOrder Decoded JSON payload: {:?}", json_payload);

            let get_order = data.get(&json_payload.id);

            if let Some(order) = get_order {
                let res = Lsps1ValidateAndPay {
                    order: order.to_string(),
                    client,
                    order_response_payload: json_payload,
                }
                .validate_and_pay()
                .await;

                match res {
                    Ok(_) => {
                        log::info!("Order validated and paid");
                    }
                    Err(e) => {
                        log::error!("Order validation and payment failed: {}", e);
                    }
                }
            }
        }
        Err(e) => {
            log::warn!("CreateOrder Failed to decode JSON payload: {}", e);
        }
    };

    return Ok(json!({ "result": "continue" }));
}
