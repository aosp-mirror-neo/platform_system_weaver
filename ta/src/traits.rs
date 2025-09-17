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

//! Traits representing abstractions of device-specific functionality.

use crate::{Error, Slot};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use hal_ta::traits::{ConstTimeEq, MonotonicClock};
use hal_wire::AsCborValue;

/// Combined collection of trait implementations that must be provided.
pub struct Implementation {
    /// A local clock.
    pub clock: Box<dyn MonotonicClock>,
    /// A constant-time equality implementation.
    pub compare: Box<dyn ConstTimeEq>,
    /// Trait for persistent slot storage.
    pub storage: Box<dyn SlotStorage>,
}

/// Abstraction of the device-specific functionality for storing slots.
/// Slots must be persistent across boots and across untrusted factory resets.
pub trait SlotStorage {
    /// Retrieves the slot data that was stored under `slot_id` by `write_slot`. If `write_slot` has
    /// never been called for the provided `slot_id`, return `Err(Error::InvalidArgument)`.
    fn read_slot(&self, slot_id: i32) -> Result<Slot, Error>;
    /// Stores the `slot` data under the given `slot_id`.
    fn write_slot(&mut self, slot_id: i32, slot: &Slot) -> Result<(), Error>;
}

/// Abstraction of simple flat filesystem functionality.
pub trait SecureFilesystem {
    /// Read the entire contents of the given file.  Return `Err(Error::InvalidArgument)` if the
    /// file does not exist.
    fn read(&self, filename: &str) -> Result<Vec<u8>, Error>;
    /// Write the entire contents of the given file, overwriting any existing contents.
    fn write(&mut self, filename: &str, data: &[u8]) -> Result<(), Error>;
}

/// Generate the slot filename corresponding to a particular `slot_id`.
fn filename(slot_id: i32) -> String {
    alloc::format!("weaver.{slot_id}")
}

/// Implementation of slot storage based on an underlying flat filesystem.
impl<T> SlotStorage for T
where
    T: SecureFilesystem,
{
    fn read_slot(&self, slot_id: i32) -> Result<Slot, Error> {
        let data = self.read(&filename(slot_id))?;
        Ok(Slot::from_slice(&data)?)
    }

    fn write_slot(&mut self, slot_id: i32, slot: &Slot) -> Result<(), Error> {
        let data = slot.clone().into_vec()?;
        self.write(&filename(slot_id), &data)
    }
}
