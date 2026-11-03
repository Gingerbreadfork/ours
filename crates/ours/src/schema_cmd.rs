//! Writes the published JSON Schemas: the engine's objects and the
//! connector event model.

use std::path::Path;

pub fn write(out: &Path) -> anyhow::Result<usize> {
    std::fs::create_dir_all(out)?;
    let mut schemas = ours_core::schema::schemas();
    schemas.push((
        "inbound_event",
        schemars::schema_for!(ours_types::InboundEvent).to_value(),
    ));
    schemas.push((
        "outbound",
        schemars::schema_for!(ours_types::Outbound).to_value(),
    ));
    schemas.push((
        "capabilities",
        schemars::schema_for!(ours_types::Capabilities).to_value(),
    ));
    schemas.push((
        "outside_action",
        schemars::schema_for!(ours_types::OutsideAction).to_value(),
    ));
    schemas.push((
        "inbound_mail",
        schemars::schema_for!(ours_types::InboundMail).to_value(),
    ));
    for (name, schema) in &schemas {
        let path = out.join(format!("{name}.schema.json"));
        std::fs::write(&path, serde_json::to_string_pretty(schema)?)?;
    }
    Ok(schemas.len())
}
