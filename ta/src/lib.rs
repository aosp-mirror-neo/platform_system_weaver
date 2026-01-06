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

//! Common code implementing a Weaver TA.

#![no_std]
extern crate alloc;

use alloc::vec::Vec;
use hal_wire::types::MillisecondsSinceEpoch;
use hal_wire::{AsCborValue, CborError};
use hal_wire_derive::AsCborValue;
use log::{error, info, trace, warn};
use weaver_wire as wire;
use wire::{
    ApiStatus, GetConfigResponse, PerformOpReq, PerformOpResponse, PerformOpRsp, ReadResponse,
    SlotKey, SlotValue, WeaverOperation, WriteResponse,
};

pub mod traits;

/// Errors encountered in TA processing.
#[derive(Debug)]
pub enum Error {
    /// Error from CBOR conversion.
    Cbor(CborError),
    /// Memory allocation failure.
    AllocationFailed,
    /// File not found.
    FileNotFound,
    /// Provided argument(s) are invalid.
    InvalidArgument,
    /// Incorrect key provided.
    IncorrectKey,
    /// Operation cannot be performed until given number of milliseconds have passed.
    Throttle(i64),
}

impl From<alloc::collections::TryReserveError> for Error {
    fn from(_e: alloc::collections::TryReserveError) -> Self {
        Error::AllocationFailed
    }
}

impl From<CborError> for Error {
    fn from(e: CborError) -> Self {
        Error::Cbor(e)
    }
}

/// Represents an individual Weaver slot.
#[derive(AsCborValue, Clone, Default)]
pub struct Slot {
    /// The timestamp of the last check.
    last_checked_timestamp: MillisecondsSinceEpoch,
    /// Number of failed reads.
    failure_counter: u32,
    slot_key: SlotKey,
    slot_value: SlotValue,
}

/// Weaver device implementation, running in secure environment.
pub struct WeaverTa {
    /// Device-specific trait implementation.  Fixed on construction.
    imp: traits::Implementation,
    num_slots: i32,
    key_size: i32,
    value_size: i32,
}

const ONE_MINUTE: i64 = 60000;
const ONE_HOUR: i64 = 60 * ONE_MINUTE;
const ONE_DAY: i64 = 24 * ONE_HOUR;
const ONE_YEAR: i64 = 365 * ONE_DAY; // intentionally not considering leap years

/// Array that maps values of the failure counter to the timeout (in milliseconds) that the
/// Weaver TA enforces after that number of failures.
const TIMEOUT_TABLE: &[i64] = &[
    /* 0  */ 0,
    /* 1  */ 0,
    /* 2  */ 0,
    /* 3  */ 0,
    /* 4  */ 0,
    /* 5  */ ONE_MINUTE,
    /* 6  */ 5 * ONE_MINUTE,
    /* 7  */ 15 * ONE_MINUTE,
    /* 8  */ 30 * ONE_MINUTE,
    /* 9  */ 90 * ONE_MINUTE,
    /* 10 */ 4 * ONE_HOUR,
    /* 11 */ 12 * ONE_HOUR,
    /* 12 */ 36 * ONE_HOUR,
    /* 13 */ 4 * ONE_DAY,
    /* 14 */ 13 * ONE_DAY,
    /* 15 */ 41 * ONE_DAY,
    /* 16 */ 123 * ONE_DAY,
    /* 17 */ ONE_YEAR,
    /* 18 */ 3 * ONE_YEAR,
    /* 19 */ 9 * ONE_YEAR,
];

impl WeaverTa {
    /// Create a new [`WeaverTa`] instance.
    pub fn new(
        imp: traits::Implementation,
        num_slots: i32,
        key_size: i32,
        value_size: i32,
    ) -> Self {
        if num_slots < 64 || key_size < 16 || value_size < 16 {
            panic!("num_slots, key_size or value_size below minimum");
        }
        Self { imp, num_slots, key_size, value_size }
    }

    /// Process a single serialized request, returning a serialized response.
    pub fn process(&mut self, req_data: &[u8]) -> Vec<u8> {
        let (req_code, rsp) = match PerformOpReq::from_slice(req_data) {
            Ok(req) => {
                trace!("-> TA: received request {:?}", req.code());
                (Some(req.code()), self.process_req(req))
            }
            Err(e) => {
                error!("failed to decode CBOR request: {e:?}");
                (None, PerformOpResponse::Err(ApiStatus::Failed as i32))
            }
        };
        trace!("<- TA: send response {req_code:?} rc {:?}", rsp.error_code());
        match rsp.into_vec() {
            Ok(rsp_data) => rsp_data,
            Err(e) => {
                error!("failed to encode CBOR response: {e:?}");
                INVALID_CBOR_RSP_DATA.to_vec()
            }
        }
    }

    /// Process a single request, returning a [`PerformOpResponse`].
    ///
    /// Select the appropriate method based on the request type, and use the
    /// request fields as parameters to the method.  In the opposite direction,
    /// build a response message from the values returned by the method.
    fn process_req(&mut self, req: PerformOpReq) -> PerformOpResponse {
        let req_code = req.code();
        match req {
            PerformOpReq::GetConfig(_) => {
                PerformOpResponse::Ok(PerformOpRsp::GetConfig(GetConfigResponse {
                    num_slots: self.num_slots,
                    key_size: self.key_size,
                    value_size: self.value_size,
                }))
            }
            PerformOpReq::Read(req) => match self.read(req.slot_id, &req.key) {
                Ok(value) => PerformOpResponse::Ok(PerformOpRsp::Read(ReadResponse { value })),
                Err(e) => error_rsp(req_code, e),
            },
            PerformOpReq::Write(req) => match self.write(req.slot_id, req.key, req.value) {
                Ok(_) => PerformOpResponse::Ok(PerformOpRsp::Write(WriteResponse {})),
                Err(e) => error_rsp(req_code, e),
            },
        }
    }

    fn read(&mut self, slot_id: i32, slot_key: &SlotKey) -> Result<SlotValue, Error> {
        info!("read from slot {slot_id}");

        self.verify_slot_id(slot_id)?;
        self.verify_key_length(slot_key)?;

        let mut slot = self.imp.storage.read_slot(slot_id)?;
        let now = self.imp.clock.now();

        // Give up early if the user is throttled.
        if let Some(t) = self.throttle_time_remaining(slot_id, &slot, now) {
            return Err(Error::Throttle(t));
        }

        // Pre-increment the failure count just in case (success later will clear it).
        slot.failure_counter = slot.failure_counter.saturating_add(1);
        slot.last_checked_timestamp = now;
        self.imp.storage.write_slot(slot_id, &slot)?;

        if self.imp.compare.eq(slot_key, &slot.slot_key) {
            // Reset the failure count on success.
            slot.failure_counter = 0;
            self.imp.storage.write_slot(slot_id, &slot)?;
            Ok(slot.slot_value)
        } else {
            Err(Error::IncorrectKey)
        }
    }

    fn write(
        &mut self,
        slot_id: i32,
        slot_key: SlotKey,
        slot_value: SlotValue,
    ) -> Result<(), Error> {
        info!("write to slot {slot_id}");

        self.verify_slot_id(slot_id)?;
        self.verify_key_length(&slot_key)?;
        self.verify_value_length(&slot_value)?;

        let slot = Slot {
            last_checked_timestamp: MillisecondsSinceEpoch(0),
            failure_counter: 0,
            slot_key,
            slot_value,
        };

        self.imp.storage.write_slot(slot_id, &slot)
    }

    /// Returns the remaining milliseconds before the throttle period ends.
    fn throttle_time_remaining(
        &mut self,
        slot_id: i32,
        slot: &Slot,
        now: MillisecondsSinceEpoch,
    ) -> Option<i64> {
        let timeout = compute_retry_timeout(slot.failure_counter);
        if timeout == 0 {
            return None;
        }

        // There is a pending throttle period, so see if `now` is within it.
        let lockout_end_time = slot.last_checked_timestamp + timeout;
        if now < slot.last_checked_timestamp {
            warn!(
                "now {now:?} appears before last check {:?} for slot {slot_id}",
                slot.last_checked_timestamp
            );
            Some(timeout)
        } else if now < lockout_end_time {
            let remaining = lockout_end_time - now;
            info!("in throttle period for slot {slot_id}, {remaining}ms remaining");
            Some(remaining)
        } else {
            info!("throttle period for slot {slot_id} expired by {now:?}");
            None
        }
    }

    fn verify_slot_id(&self, slot_id: i32) -> Result<(), Error> {
        if (0..self.num_slots).contains(&slot_id) {
            Ok(())
        } else {
            error!("slot id {slot_id} is out of range: [0, {})", self.num_slots);
            Err(Error::InvalidArgument)
        }
    }

    fn verify_key_length(&self, slot_key: &SlotKey) -> Result<(), Error> {
        let expected_len = self.key_size as usize;
        if slot_key.len() == expected_len {
            Ok(())
        } else {
            error!(
                "key size {} does not match expected key size: {}",
                slot_key.len(),
                self.key_size
            );
            Err(Error::InvalidArgument)
        }
    }

    fn verify_value_length(&self, slot_value: &SlotValue) -> Result<(), Error> {
        let expected_len = self.value_size as usize;
        if slot_value.len() == expected_len {
            Ok(())
        } else {
            error!(
                "value size {} does not match expected value size: {}",
                slot_value.len(),
                self.value_size
            );
            Err(Error::InvalidArgument)
        }
    }
}

fn error_rsp(op: WeaverOperation, err: Error) -> PerformOpResponse {
    warn!("failing {op:?} request with error {err:?}");
    match err {
        Error::Throttle(timeout) => PerformOpResponse::Throttle(timeout),
        Error::IncorrectKey => PerformOpResponse::Err(ApiStatus::IncorrectKey as i32),
        _ => PerformOpResponse::Err(ApiStatus::Failed as i32),
    }
}

/// Hand-encoded [`PerformOpResponse`] data for [`ApiStatus::Failed`].
/// Does not perform CBOR serialization (and so is suitable for error reporting if/when
/// CBOR serialization fails).
const INVALID_CBOR_RSP_DATA: [u8; 3] = [
    0x82, // 2-arr
    0x01, // nint, val 1 (Failed)
    0x01, // nint, val 1 (Failed)
];

/// Computes the next timeout, in milliseconds, based on the failure count.
fn compute_retry_timeout(failure_count: u32) -> i64 {
    match TIMEOUT_TABLE.get(failure_count as usize) {
        Some(timeout) => *timeout,
        None => {
            info!("no more attempts allowed after {failure_count} failures");
            i64::MAX
        }
    }
}
