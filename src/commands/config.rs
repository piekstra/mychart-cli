//! `mychart config path|show|set|unset`.

use pk_cli_core::{output, CliError};

use super::Ctx;
use crate::cli::ConfigCmd;
use crate::config::Config;

pub fn run(ctx: &Ctx, cmd: &ConfigCmd) -> Result<(), CliError> {
    match cmd {
        ConfigCmd::Path => {
            let path = ctx.store.path()?.display().to_string();
            output::emit(
                ctx.json(),
                "config-path",
                serde_json::json!({ "path": path }),
                |p| println!("{}", p["path"].as_str().unwrap_or_default()),
            );
            Ok(())
        }
        ConfigCmd::Show => {
            let cfg: Config = ctx.config()?;
            let v = super::to_json(&cfg)?;
            if ctx.json() {
                output::json(&output::tagged("config", v));
            } else if v.as_object().is_some_and(|o| o.is_empty()) {
                eprintln!("no settings (`mychart config set client_id <ID>`)");
            } else {
                output::render(&v);
            }
            Ok(())
        }
        ConfigCmd::Set { key, value } => {
            let mut cfg: Config = ctx.config()?;
            cfg.set(key, value)?;
            ctx.store.save(&cfg)?;
            ctx.note(format!("{key} set"));
            Ok(())
        }
        ConfigCmd::Unset { key } => {
            let mut cfg: Config = ctx.config()?;
            cfg.unset(key)?;
            ctx.store.save(&cfg)?;
            ctx.note(format!("{key} unset"));
            Ok(())
        }
    }
}
