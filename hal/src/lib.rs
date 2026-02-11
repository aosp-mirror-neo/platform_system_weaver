// Copyright 2025, The Android Open Source Project
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Implementation of a HAL service for Weaver.
//!
//! This implementation relies on a `SerializedChannel` abstraction for a communication channel to
//! the trusted application (TA).  Incoming method invocations for the HAL service are converted
//! into corresponding request structures, which are then serialized (using CBOR) and sent down the
//! channel.  A serialized response is then read from the channel, which is deserialized into a
//! response structure.  The contents of this response structure are then used to populate the
//! return values of the HAL service method.

use android_hardware_weaver::aidl::android::hardware::weaver::{
    IWeaver::{BnWeaver, IWeaver, STATUS_FAILED},
    WeaverConfig::WeaverConfig,
    WeaverReadResponse::WeaverReadResponse,
    WeaverReadStatus::WeaverReadStatus,
};
use hal_hal::channel::{ChannelHalService, SerializedChannel};
use log::warn;
use std::sync::{Arc, Mutex, MutexGuard};
use weaver_wire as wire;
use wire::{PerformOpReq, PerformOpResponse, PerformOpRsp, SlotKey, SlotValue};

/// Implementation of the `IWeaver` HAL service, communicating with a TA
/// in a secure environment via a communication channel.
pub struct WeaverService<T: SerializedChannel + 'static> {
    channel: Arc<Mutex<T>>,
}

impl<T: SerializedChannel + 'static> WeaverService<T> {
    /// Construct a new instance that uses the provided channel.
    pub fn new(channel: Arc<Mutex<T>>) -> Self {
        Self { channel }
    }
    /// Create a new instance wrapped in a proxy object.
    pub fn new_as_binder(channel: Arc<Mutex<T>>) -> binder::Strong<dyn IWeaver> {
        BnWeaver::new_binder(Self::new(channel), binder::BinderFeatures::default())
    }
}

impl<T: SerializedChannel> ChannelHalService<T> for WeaverService<T> {
    fn channel(&self) -> MutexGuard<T> {
        self.channel.lock().unwrap()
    }
}

impl<T: SerializedChannel + Send> binder::Interface for WeaverService<T> {}

/// Implement the `IWeaver` interface by translating incoming method invocations into request
/// messages, which are passed to the TA.  The corresponding response from the TA is then parsed and
/// the return values extracted.
impl<T: SerializedChannel> IWeaver for WeaverService<T> {
    fn getConfig(&self) -> binder::Result<WeaverConfig> {
        const OP: &str = "getConfig";
        let req = PerformOpReq::GetConfig(wire::GetConfigRequest {});
        match self.execute_req(req)? {
            PerformOpResponse::Ok(PerformOpRsp::GetConfig(rsp)) => Ok(WeaverConfig {
                slots: rsp.num_slots,
                keySize: rsp.key_size,
                valueSize: rsp.value_size,
            }),
            PerformOpResponse::Err => Err(weaver_err_to_binder(OP)),
            _ => Err(binder::Status::new_exception(binder::ExceptionCode::ILLEGAL_STATE, None)),
        }
    }

    fn read(&self, slot_id: i32, key: &[u8]) -> binder::Result<WeaverReadResponse> {
        let req = PerformOpReq::Read(wire::ReadRequest { slot_id, key: SlotKey(key.to_vec()) });
        match self.execute_req(req)? {
            PerformOpResponse::Ok(PerformOpRsp::Read(rsp)) => Ok(WeaverReadResponse {
                status: WeaverReadStatus::OK,
                timeout: 0,
                value: rsp.value.0.clone(),
            }),
            PerformOpResponse::Ok(_) => {
                Err(binder::Status::new_exception(binder::ExceptionCode::ILLEGAL_STATE, None))
            }
            PerformOpResponse::Throttle(timeout) => Ok(WeaverReadResponse {
                status: WeaverReadStatus::THROTTLE,
                timeout,
                value: Vec::new(),
            }),
            PerformOpResponse::IncorrectKey(timeout) => Ok(WeaverReadResponse {
                status: WeaverReadStatus::INCORRECT_KEY,
                timeout,
                value: Vec::new(),
            }),
            PerformOpResponse::Err => Ok(WeaverReadResponse {
                status: WeaverReadStatus::FAILED,
                timeout: 0,
                value: Vec::new(),
            }),
        }
    }

    fn write(&self, slot_id: i32, key: &[u8], value: &[u8]) -> binder::Result<()> {
        const OP: &str = "write";
        let req = PerformOpReq::Write(wire::WriteRequest {
            slot_id,
            key: SlotKey(key.to_vec()),
            value: SlotValue(value.to_vec()),
        });
        match self.execute_req(req)? {
            PerformOpResponse::Ok(PerformOpRsp::Write(_rsp)) => Ok(()),
            PerformOpResponse::Err => Err(weaver_err_to_binder(OP)),
            _ => Err(binder::Status::new_exception(binder::ExceptionCode::ILLEGAL_STATE, None)),
        }
    }
}

/// Convert a Weaver error code into a binder error.
pub fn weaver_err_to_binder(op: &str) -> binder::Status {
    warn!("{op} failed: {STATUS_FAILED}");
    binder::Status::new_service_specific_error(STATUS_FAILED, None)
}
