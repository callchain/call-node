//! Agent instruction callback type.

use crate::instructions::types::{Instruction, InstructionResult};
use crate::{ProtocolResult};
use call_primitives::Address;

/// Callback type for executing agent instructions.
///
/// The agent layer implements this to provide actual agent balance/permission logic.
/// If the closure returns `Some(result)`, that result is used directly.
/// If it returns `None`, the instruction falls back to default handling.
pub type AgentExecutor<'a> = &'a mut dyn FnMut(&Instruction, Address) -> Option<ProtocolResult<InstructionResult>>;
