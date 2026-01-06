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

//! Types used for communication between HAL and TA.
//!
//! The HAL code receives data encoded in the AIDL-defined types, but the
//! TA code doesn't use the same types (because that would require access to
//! the AIDL-generated code in the build system for the secure environment).
//!
//! So instead use types defined in this crate, which the HAL service can
//! translate to/from.

// Allow missing docs in this crate as the types here are generally 1:1 with the HAL
// interface definitions.
#![allow(missing_docs)]
#![no_std]
extern crate alloc;

/// Re-export of crate used for CBOR encoding.
pub use ciborium as cbor;

use alloc::vec;
use alloc::vec::Vec;
use cbor::value::Value;
use core::ops::Deref;
use enumn::N;
use hal_wire::{declare_req_rsp_enums, AsCborValue, CborError};
use hal_wire_derive::AsCborValue;
use zeroize::ZeroizeOnDrop;

/// Error codes defined in the Weaver HAL.
#[derive(Debug, N, AsCborValue)]
pub enum ApiStatus {
    Ok = 0,
    Failed = 1,
    IncorrectKey = 2,
    Throttle = 3,
}

/// A slot key.
#[derive(Clone, Default, ZeroizeOnDrop)] // deliberately not `Debug`
pub struct SlotKey(pub Vec<u8>);

impl Deref for SlotKey {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsCborValue for SlotKey {
    fn to_cbor_value(mut self) -> Result<Value, CborError> {
        let inner = core::mem::take(&mut self.0);
        inner.to_cbor_value()
    }

    fn from_cbor_value(value: Value) -> Result<Self, CborError> {
        Vec::<u8>::from_cbor_value(value).map(SlotKey)
    }
}

/// A slot value.
#[derive(Clone, Default, ZeroizeOnDrop)] // deliberately not `Debug`
pub struct SlotValue(pub Vec<u8>);

impl Deref for SlotValue {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsCborValue for SlotValue {
    fn to_cbor_value(mut self) -> Result<Value, CborError> {
        let inner = core::mem::take(&mut self.0);
        inner.to_cbor_value()
    }

    fn from_cbor_value(value: Value) -> Result<Self, CborError> {
        Vec::<u8>::from_cbor_value(value).map(SlotValue)
    }
}

#[derive(AsCborValue)]
pub struct GetConfigRequest {}

#[derive(AsCborValue)]
pub struct GetConfigResponse {
    pub num_slots: i32,
    pub key_size: i32,
    pub value_size: i32,
}

#[derive(AsCborValue)]
pub struct ReadRequest {
    pub slot_id: i32,
    pub key: SlotKey,
}

#[derive(AsCborValue)]
pub struct ReadResponse {
    pub value: SlotValue,
}

#[derive(AsCborValue)]
pub struct WriteRequest {
    pub slot_id: i32,
    pub key: SlotKey,
    pub value: SlotValue,
}

#[derive(AsCborValue)]
pub struct WriteResponse {}

// Possible operation requests, as:
// - an enum value with an explicit numeric value
// - a request enum which has an operation code associated to each variant
// - a response enum which has the same operation code associated to each variant.
declare_req_rsp_enums! { WeaverOperation  => (PerformOpReq, PerformOpRsp) {
    GetConfig = 0x01 => (GetConfigRequest, GetConfigResponse),
    Read = 0x02 => (ReadRequest, ReadResponse),
    Write = 0x03 => (WriteRequest, WriteResponse),
} }

/// Outer response message type that allows for failure.
pub enum PerformOpResponse {
    /// An OK response with inner response message (equivalent to [`ApiStatus::Ok`]).
    Ok(PerformOpRsp),
    /// A response indicating that the request should be retried after the given number of
    /// milliseconds (equivalent to [`ApiStatus::Throttle`]).
    Throttle(i64),
    /// General error response. The contained error code is context-specific.
    Err(i32),
}

impl AsCborValue for PerformOpResponse {
    fn from_cbor_value(value: Value) -> Result<Self, CborError> {
        let mut arr = match value {
            Value::Array(a) if a.len() == 2 => a,
            Value::Array(_) => return Err(CborError::UnexpectedItem("arr not len 2", "arr len 2")),
            _ => return Err(CborError::UnexpectedItem("non-arr", "arr")),
        };
        let value = arr.remove(1);
        let code = ApiStatus::from_cbor_value(arr.remove(0))?;
        match code {
            ApiStatus::Ok => {
                let rsp = PerformOpRsp::from_cbor_value(value)?;
                Ok(Self::Ok(rsp))
            }
            ApiStatus::Throttle => {
                let timeout = i64::from_cbor_value(value)?;
                Ok(Self::Throttle(timeout))
            }
            ApiStatus::Failed | ApiStatus::IncorrectKey => {
                let rc = i32::from_cbor_value(value)?;
                Ok(Self::Err(rc))
            }
        }
    }
    fn to_cbor_value(self) -> Result<Value, CborError> {
        let (code, value) = match self {
            Self::Ok(rsp) => (ApiStatus::Ok, rsp.to_cbor_value()?),
            Self::Throttle(timeout) => (ApiStatus::Throttle, timeout.to_cbor_value()?),
            // For Err, we use the raw i32 to find the best ApiStatus discriminant, defaulting to
            // Failed. The second element is the raw i32 error code itself.
            Self::Err(rc) => (ApiStatus::n(rc).unwrap_or(ApiStatus::Failed), rc.to_cbor_value()?),
        };
        Ok(Value::Array(vec![code.to_cbor_value()?, value]))
    }
}

impl PerformOpResponse {
    /// Return the status code associated with the response.
    pub fn error_code(&self) -> ApiStatus {
        match self {
            Self::Ok(_) => ApiStatus::Ok,
            Self::Throttle(_) => ApiStatus::Throttle,
            Self::Err(status) => ApiStatus::n(*status).unwrap_or(ApiStatus::Failed),
        }
    }
}
