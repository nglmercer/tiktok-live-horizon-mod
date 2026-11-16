//! `World` on top of the real Horizon server.
use mod_api::{BlockUpdateFlags, ServerHost};

use crate::blocks::{Pos, World};

pub struct HostWorld<'a> {
    pub host: &'a mut dyn ServerHost,
}

impl World for HostWorld<'_> {
    fn is_loaded(&self, dim: &str, pos: Pos) -> bool {
        self.host.level(dim).is_some_and(|level| level.is_loaded(&*self.host, pos))
    }

    fn height(&self, dim: &str) -> Option<(i32, i32)> {
        let level = self.host.level(dim)?;
        let (min, height) = level.height(&*self.host)?;
        Some((min, min + height))
    }

    fn state(&self, dim: &str, pos: Pos) -> Option<String> {
        let level = self.host.level(dim)?;
        let state = level.block_state(&*self.host, pos)?;
        self.host.block_state_string(state)
    }

    fn set_state(&mut self, dim: &str, pos: Pos, state: &str) -> bool {
        let Some(level) = self.host.level(dim) else { return false };
        let Some(id) = self.host.block_state_id(state) else { return false };
        // The plugin's `setType(.., false)`: clients are notified but no physics are triggered.
        matches!(level.set_block(&mut *self.host, pos, id, BlockUpdateFlags::CLIENTS), Ok(true))
    }

    fn valid_block(&self, id: &str) -> bool {
        self.host.block_state_id(id).is_some()
    }
}
