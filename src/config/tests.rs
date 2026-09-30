// Copyright 2026 David Akermann
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use super::*;

#[cfg(unix)]
#[test]
fn creates_config_file_with_owner_only_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    create_if_missing(&path).unwrap();
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn accepts_the_complete_example() {
    let config: Config = toml::from_str(include_str!("../../agent.example.toml")).unwrap();
    config.validate().unwrap();
    assert!(config.mcp_servers.is_empty());
    assert!(config.enabled_provider().is_none());
}

#[test]
fn rejects_duplicate_provider_names_even_when_disabled() {
    let mut source: toml::Table = toml::from_str(include_str!("../../agent.example.toml")).unwrap();
    let providers = source.get_mut("providers").unwrap().as_array_mut().unwrap();
    providers.push(providers[0].clone());
    let config: Config = source.try_into().unwrap();
    assert!(
        config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("duplicate provider_name: deepseek")
    );
}

#[test]
fn rejects_unknown_enabled_provider() {
    let mut config: Config = toml::from_str(include_str!("../../agent.example.toml")).unwrap();
    config.providers[0].provider_name = "other".into();
    config.providers[0].enabled = true;
    assert!(
        config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("unsupported provider: other")
    );
}

#[test]
fn requires_every_provider_field() {
    let source: toml::Table = toml::from_str(include_str!("../../agent.example.toml")).unwrap();
    let source = source["providers"].as_array().unwrap()[0]
        .as_table()
        .unwrap();
    for field in ["provider_name", "enabled", "model"] {
        let mut incomplete = source.clone();
        incomplete.remove(field);
        let error = toml::from_str::<Provider>(&incomplete.to_string())
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains(&format!("missing field `{field}`")),
            "{error}"
        );
    }
}

#[test]
fn requires_every_mcp_field_but_accepts_explicit_empty_values() {
    let source: toml::Table =
        toml::from_str("name = 'test'\ncommand = 'server'\nargs = []\nenv = {}").unwrap();
    let server: McpServer = toml::from_str(&source.to_string()).unwrap();
    assert!(server.args.is_empty());
    assert!(server.env.is_empty());
    for field in ["name", "command", "args", "env"] {
        let mut incomplete = source.clone();
        incomplete.remove(field);
        let error = toml::from_str::<McpServer>(&incomplete.to_string())
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains(&format!("missing field `{field}`")),
            "{error}"
        );
    }
}

#[test]
fn validates_explicit_values_and_rejects_unknown_fields() {
    let source: toml::Table = toml::from_str(include_str!("../../agent.example.toml")).unwrap();
    for (field, value) in [
        ("max_turns", toml::Value::Integer(0)),
        ("model", toml::Value::String(String::new())),
        (
            "base_url",
            toml::Value::String("http://localhost/v1".into()),
        ),
        ("provider", toml::Value::String("other".into())),
    ] {
        let mut invalid = source.clone();
        invalid.insert(field.into(), value);
        let parsed = toml::from_str::<Config>(&invalid.to_string());
        assert!(
            parsed
                .and_then(|c| c.validate().map_err(serde::de::Error::custom))
                .is_err()
        );
    }
}

fn llama(extra: &str) -> Provider {
    toml::from_str(&format!(
        "provider_name='llamacpp'\nenabled=false\nmodel='local-agent'\n{extra}"
    ))
    .unwrap()
}
#[test]
fn llama_defaults_and_paths() {
    let provider = llama("");
    provider.validate_selected().unwrap();
    assert!(provider.api_key.is_empty());
    assert_eq!(
        provider.llamacpp_endpoint().unwrap(),
        "http://127.0.0.1:8080/v1/chat/completions"
    );
    assert_eq!(provider.request_timeout_secs.unwrap_or(600), 600);
    for base in [
        "http://localhost:8080/proxy/v1",
        "http://localhost:8080/proxy/v1/",
    ] {
        assert_eq!(
            llama(&format!("base_url='{base}'"))
                .llamacpp_endpoint()
                .unwrap(),
            "http://localhost:8080/proxy/v1/chat/completions"
        );
    }
    llama("base_url='https://remote.example/v1'\napi_key='key'\nrequest_timeout_secs=10")
        .validate_selected()
        .unwrap();
}
#[test]
fn invalid_llama_settings() {
    for url in [
        "",
        "not-url",
        "file:///tmp/model",
        "ftp://host/v1",
        "http://user:secret@host/v1",
        "http://host/v1?key=secret",
        "http://host/v1#part",
    ] {
        assert!(
            llama(&format!("base_url='{url}'"))
                .validate_selected()
                .is_err(),
            "{url}"
        );
    }
    assert!(llama("request_timeout_secs=0").validate_selected().is_err());
    assert!(
        toml::from_str::<Provider>(
            "provider_name='llamacpp'\nenabled=true\nmodel='m'\nrequest_timeout_secs=-1"
        )
        .is_err()
    );
    let mut provider = llama("");
    provider.model.clear();
    assert!(provider.validate_selected().is_err());
}
#[test]
fn overrides_and_ambiguity() {
    let mut config: Config = toml::from_str("providers=[]").unwrap();
    assert!(config.select_provider(None).unwrap().is_none());
    assert!(config.select_provider(Some("llamacpp")).is_err());
    config.providers.push(llama(""));
    assert!(
        !config
            .select_provider(Some("llamacpp"))
            .unwrap()
            .unwrap()
            .enabled
    );
    config.providers[0].model.clear();
    assert!(config.select_provider(Some("llamacpp")).is_err());
    config.providers[0].model = "local-agent".into();
    config.providers[0].enabled = true;
    config.providers.push(
        toml::from_str(
            "provider_name='deepseek'\nenabled=true\nmodel='deepseek-flash'\napi_key='key'",
        )
        .unwrap(),
    );
    assert!(
        config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("multiple enabled")
    );
    config.providers[1].enabled = false;
    config.validate().unwrap();
    assert_eq!(
        config
            .select_provider(Some("deepseek"))
            .unwrap()
            .unwrap()
            .provider_name,
        "deepseek"
    );
}
#[test]
fn deepseek_still_requires_key_and_rejects_local_settings() {
    let mut provider: Provider =
        toml::from_str("provider_name='deepseek'\nenabled=false\nmodel='deepseek-flash'").unwrap();
    assert!(
        provider
            .validate_selected()
            .unwrap_err()
            .to_string()
            .contains("api_key")
    );
    provider.api_key = "key".into();
    provider.base_url = Some("http://localhost/v1".into());
    assert!(provider.validate_selected().is_err());
    provider.base_url = None;
    provider.request_timeout_secs = Some(600);
    assert!(provider.validate_selected().is_err());
}
