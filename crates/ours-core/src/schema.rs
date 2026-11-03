//! The engine's typed objects as JSON Schema, for the published schema
//! directory and for anything that reads the event log.

use crate::action::{ActionRecord, CatalogueEntry};
use crate::effect::Effect;
use crate::export::ExportData;
use crate::input::{Command, Input};
use crate::plan::Plan;
use crate::state::GroupState;

/// Each published type's file stem and schema.
pub fn schemas() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("input", schemars::schema_for!(Input).to_value()),
        ("effect", schemars::schema_for!(Effect).to_value()),
        ("command", schemars::schema_for!(Command).to_value()),
        ("plan", schemars::schema_for!(Plan).to_value()),
        ("group_state", schemars::schema_for!(GroupState).to_value()),
        (
            "action_record",
            schemars::schema_for!(ActionRecord).to_value(),
        ),
        (
            "catalogue_entry",
            schemars::schema_for!(CatalogueEntry).to_value(),
        ),
        ("export", schemars::schema_for!(ExportData).to_value()),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_schema_is_an_object() {
        for (name, schema) in super::schemas() {
            assert!(schema.is_object(), "{name}");
            assert!(schema.get("title").is_some(), "{name}");
        }
    }
}
