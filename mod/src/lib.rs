//! TikTok LIVE for Horizon: native migration of the `commad-tiktok-mod` (Paper) plugin.
//!
//! Golden rule: the `host` is only used inside a hook. What comes from outside (network)
//! is written to `Hub::inbox` and processed in `server_tick`.
use std::sync::Mutex;

use mod_api::{
    self, Command, CommandArgumentValue, Config, ConfigKind, Flow, HostSide, Hooks, RegisterCommandsEvent,
    RegisterConfigsEvent, RunModCommandEvent, ServerHost, ServerStartedEvent, ServerTickEvent, ServerTickPhase, Text,
};

mod actions;
mod admin;
mod blocks;
mod chat;
mod commands;
mod config;
mod defaults;
mod dsl;
mod event;
mod functions;
#[cfg(test)]
mod golden;
mod http;
mod live;
mod hostworld;
mod hub;
mod rules;
mod scheduler;
mod value;
mod yamledit;

use hub::{Hub, CONFIG_PATH, MOD_ID};

#[derive(Default)]
struct Mod {
    hub: Mutex<Hub>,
}

impl Mod {
    fn hub(&self) -> std::sync::MutexGuard<'_, Hub> {
        // A panic in another hook must not leave the mod unusable.
        self.hub.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn argument_text(event: &RunModCommandEvent<'_>, name: &str) -> Option<String> {
    match event.command.argument(name)? {
        CommandArgumentValue::String(s) => Some(s.clone()),
        CommandArgumentValue::Integer(i) => Some(i.to_string()),
        CommandArgumentValue::Number(n) => Some(n.to_string()),
        CommandArgumentValue::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

#[mod_api::horizon_mod]
impl Hooks for Mod {
    fn register_configs(&self, _host: HostSide<'_>, event: &mut RegisterConfigsEvent<'_>) {
        event.configs.push(Config::new(MOD_ID, ConfigKind::Common, config::DEFAULT_CONFIG).file(CONFIG_PATH));
    }

    fn config_loading(&self, _host: HostSide<'_>, event: &mut mod_api::ConfigLoadingEvent<'_>) {
        if event.config.mod_id == MOD_ID {
            self.hub().request_reload();
        }
    }

    fn config_reloading(&self, _host: HostSide<'_>, event: &mut mod_api::ConfigReloadingEvent<'_>) {
        if event.config.mod_id == MOD_ID {
            self.hub().request_reload();
        }
    }

    fn register_commands(&self, _host: &mut dyn ServerHost, event: &mut RegisterCommandsEvent<'_>) {
        for usage in commands::USAGES {
            event.commands.push(Command::op(usage, 4));
        }
    }

    fn run_mod_command(&self, host: &mut dyn ServerHost, event: &mut RunModCommandEvent<'_>) -> Flow {
        let words: Vec<String> = event.command.literals.clone();
        if words.first().map(String::as_str) != Some("tiktok") {
            return Flow::Continue;
        }
        let lines = {
            let arg = |name: &str| argument_text(event, name);
            let mut hub = self.hub();
            commands::run(&mut hub, host, &words, &arg)
        };
        for line in lines {
            let _ = host.commands().feedback(Text::literal(line));
        }
        Flow::Cancel
    }

    fn server_started(&self, _host: &mut dyn ServerHost, _event: &mut ServerStartedEvent) {
        self.hub().request_reload();
    }

    fn player_logged_in(&self, host: &mut dyn ServerHost, event: &mut mod_api::PlayerLoggedInEvent<'_>) {
        let name = event.name.to_string();
        self.hub().on_player_joined(host, &name);
    }

    fn server_stopping(&self, host: &mut dyn ServerHost, _event: &mut mod_api::ServerStoppingEvent) {
        let mut hub = self.hub();
        hub.restore_world(host);
        hub.shutdown();
    }

    fn server_tick(&self, host: &mut dyn ServerHost, event: &mut ServerTickEvent<'_>) {
        if matches!(event.phase, ServerTickPhase::ServerTickStart { .. }) {
            self.hub().on_tick(host);
        }
    }
}
