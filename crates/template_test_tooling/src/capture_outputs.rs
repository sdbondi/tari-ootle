//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::sync::{Arc, Mutex};

use tari_engine::runtime::{RuntimeModule, RuntimeModuleError, StateTracker};
use tari_engine_types::indexed_value::IndexedValue;

/// Copies what each instruction of a transaction produces, in execution order, fee intent first. An
/// instruction that produces nothing is recorded as unit, so the entries line up with the instructions.
#[derive(Debug, Clone, Default)]
pub struct CaptureOutputsModule {
    outputs: Arc<Mutex<Vec<IndexedValue>>>,
}

impl CaptureOutputsModule {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn take(&self) -> Vec<IndexedValue> {
        std::mem::take(&mut *self.outputs.lock().unwrap())
    }
}

impl<TStore> RuntimeModule<TStore> for CaptureOutputsModule {
    fn on_instruction_output(
        &self,
        _track: &StateTracker<TStore>,
        output: Option<&IndexedValue>,
    ) -> Result<(), RuntimeModuleError> {
        self.outputs
            .lock()
            .unwrap()
            .push(output.cloned().unwrap_or_else(IndexedValue::empty));
        Ok(())
    }
}
