//! Public domain structures. Serde retains protocol extras for execution;
//! the derived output schema exposes only the declared domain facts.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

macro_rules! output {
    ($(#[$attrs:meta])* $name:ident { $($(#[$field_attrs:meta])* $field:ident : $ty:ty),* $(,)? }) => {
        #[derive(Serialize, Deserialize, JsonSchema)]
        $(#[$attrs])*
        pub struct $name {
            $(
                $(#[$field_attrs])*
                #[serde(default, skip_serializing_if="Option::is_none")]
                pub $field: Option<$ty>,
            )*
            #[serde(default, flatten)]
            #[schemars(skip)]
            pub protocol: BTreeMap<String,Value>,
        }
    };
}
output!(GuiEntry {
    name: String,
    title: String,
    url: String,
    status: String,
    taken: bool
});
output!(GuiView {
    title: String,
    #[schemars(extend("format"="uri"))]
    url: String
});
output!(#[schemars(extend("x-dl-content"="gui"))] GuiOutput {
    status:String, message:String, observation:GuiView,
    apps:Vec<GuiEntry>, windows:Vec<GuiEntry>, pages:Vec<GuiEntry>,
    discovered_windows:Vec<GuiEntry>, related_windows:Vec<GuiEntry>,
});
output!(McpContent {
    #[schemars(extend("x-dl-content"="text"))]
    text: String,
    name: String,
    #[schemars(extend("format"="uri"))]
    uri: String,
});
output!(Resource {
    name: String,
    media_type: String,
    #[schemars(extend("format"="uri"))]
    uri: String,
});
output!(#[schemars(extend("x-dl-error"="generic"))] McpOutput {
    content:Vec<McpContent>,
    #[serde(rename="structuredContent")] structured_content:Value,
    resources:Vec<Resource>,
});
output!(WebMatch { content: String });
output!(WebDocument {
    title:String,
    #[schemars(extend("format"="uri"))] url:String,
    content:String,structured_content:Value,matches:Vec<WebMatch>,notices:Vec<String>,next_offset:usize,
});
output!(WebOperation {
    operation: String,
    result: WebDocument,
    error: String
});
output!(#[schemars(extend("x-dl-content"="web"))] WebOutput {results:Vec<WebOperation>,closed:bool});
output!(
    #[schemars(extend("x-dl-content"="image"))]
    ImageOutput {
        path: String,
        media_type: String,
        byte_count: usize
    }
);
output!(AppEntry {
    name: String,
    description: String,
    enabled: bool
});
output!(#[schemars(extend("x-dl-content"="apps"))] AppsOutput {name:String,description:String,enabled:bool,app:AppEntry,apps:Vec<AppEntry>,tools:Vec<AppEntry>,status:String});
output!(SkillEntry {
    name: String,
    description: String,
    path: String
});
output!(#[schemars(extend("x-dl-content"="skills"))] SkillsOutput {skills:Vec<SkillEntry>});
output!(ActorEntry {
    name: String,
    status: String,
    #[schemars(extend("format"="uri"))]
    conversation_url: String
});
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SentActorMessage {
    pub conversation_url: String,
    pub content: String,
}
output!(#[schemars(extend("x-dl-content"="actor"))] ActorOutput {
    name:String,status:String,actors:Vec<ActorEntry>,
    #[schemars(extend("format"="uri"))] conversation_url:String,
    #[schemars(extend("x-dl-content"="markdown"))] message:String,
    sent_message:SentActorMessage,
});
output!(
    #[schemars(extend("x-dl-content"="status"))]
    SudoOutput {
        #[schemars(extend("x-dl-content"="status"))]
        status: String,
        executed: bool
    }
);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_follows_serde_names_and_omits_protocol_extras() {
        let schema = schemars::schema_for!(McpOutput).to_value();
        assert!(schema["properties"].get("structuredContent").is_some());
        assert!(schema["properties"].get("protocol").is_none());
        let value: GuiView = serde_json::from_value(
            serde_json::json!({"title":"Browser","state_id":"private-state"}),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(value).unwrap()["state_id"],
            "private-state"
        );
        let schema = schemars::schema_for!(GuiView).to_value();
        assert!(schema["properties"].get("state_id").is_none());
    }

    #[test]
    fn app_serialization_retains_values_and_derives_new_fields_without_a_registry() {
        #[derive(Serialize, JsonSchema)]
        struct NewOutput {
            #[serde(rename = "new_field")]
            value: String,
            nested: Vec<bool>,
            #[serde(skip)]
            _private: String,
        }
        let result = crate::app::AppToolExecutionResult::structured(
            "new result",
            &NewOutput {
                value: "exact value".into(),
                nested: vec![false, true],
                _private: "private bytes".into(),
            },
            None,
        )
        .unwrap();
        assert_eq!(
            result.payload,
            serde_json::json!({"new_field":"exact value","nested":[false,true]})
        );
        let schema = result.output_schema.unwrap();
        assert!(schema["properties"].get("new_field").is_some());
        assert!(schema["properties"].get("private").is_none());
        assert!(result.payload.get("output_schema").is_none());
    }
}
