//! OpenCode, which speaks ACP itself (`opencode acp`), on the providers the user set up in OpenCode.

use crate::claude::subagents::Agent;

use super::{Adapter, Reopen};
use serde_json::{Map, Value, json};

pub fn managed_compaction_config(inherited: Option<&str>, session: Option<&str>) -> Result<String, String> {
    let parse = |text: Option<&str>| -> Result<Map<String, Value>, String> {
        serde_json::from_str(text.unwrap_or("{}")).map_err(|e| format!("OpenCode inline config must be a JSON object: {e}"))
    };
    let mut config = parse(inherited)?;
    config.extend(parse(session)?);
    let compaction = config.entry("compaction").or_insert_with(|| json!({}));
    let options = compaction.as_object_mut().ok_or("OpenCode compaction config must be an object.")?;
    options.insert("auto".into(), json!(false));
    options.insert("background".into(), json!(false));
    Ok(Value::Object(config).to_string())
}

pub const ADAPTER: Adapter = Adapter {
    name: "OpenCode",
    server_name: "opencode",
    server: || super::subagent_server(Agent::OpenCode),
    reopen: Reopen::Load,
    delete_model_session: true,
    compact_command: Some("compact"),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_overrides_preserve_user_config_and_child_permissions() {
        let value: Value = serde_json::from_str(&managed_compaction_config(
            Some(r#"{"model":"provider/model","compaction":{"auto":true,"keep":{"tokens":24000}},"provider":{"local":{}}}"#),
            Some(r#"{"permission":{"*":"deny","read":"allow"}}"#),
        ).unwrap()).unwrap();
        assert_eq!(value["model"], "provider/model");
        assert_eq!(value["compaction"], json!({"auto":false,"background":false,"keep":{"tokens":24000}}));
        assert_eq!(value["permission"]["*"], "deny");
        assert!(value["provider"]["local"].is_object());
        assert!(managed_compaction_config(Some("[]"), None).is_err());
        assert!(managed_compaction_config(Some(r#"{"compaction":false}"#), None).is_err());
    }
}
