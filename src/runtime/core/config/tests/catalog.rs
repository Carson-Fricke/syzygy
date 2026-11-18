use serde_json::json;
use syzygy_agent_config::AgentCatalog;

#[test]
fn role_and_explicit_profile_select_provider_model_and_settings_together() {
    let config = json!({
        "profiles": {
            "interactive":{"provider":"chatgpt","model":"model-a","reasoning_effort":"medium"},
            "research":{"provider":"openrouter","model":"vendor/model-b"}
        },
        "agents":{"primary":{"model_profile":"interactive"},"researcher":{"model_profile":"research"}}
    });
    let catalog = AgentCatalog::from_json(&config.to_string()).unwrap();
    let primary = catalog.profile("primary", None).unwrap();
    assert_eq!(primary.provider, "chatgpt");
    assert_eq!(primary.reasoning_effort.as_deref(), Some("medium"));
    assert_eq!(catalog.profile("unknown-role", None).unwrap(), primary);
    let researcher = catalog.profile("researcher", None).unwrap();
    assert_eq!(researcher.model, "vendor/model-b");
    assert_eq!(researcher.provider, "openrouter");
    assert_eq!(
        catalog.profile("primary", Some("research")).unwrap(),
        researcher
    );
    assert!(catalog.profile("researcher", Some("missing")).is_err());
    for (key, invalid) in [
        ("agents", json!({"secondary":{"model_profile":"missing"}})),
        ("agents", json!({"":{"model_profile":"research"}})),
        (
            "profiles",
            json!({"interactive":{"provider":"  ","model":"model-a"}}),
        ),
        (
            "profiles",
            json!({"interactive":{"provider":"chatgpt","model":""}}),
        ),
        (
            "profiles",
            json!({"":{"provider":"chatgpt","model":"model-a"}}),
        ),
    ] {
        let mut bad = config.clone();
        bad[key] = invalid;
        assert!(AgentCatalog::from_json(&bad.to_string()).is_err());
    }
}
