//! Fixed sandbox paths and limits shared by review records and execution adapters.

/// Maximum ASCII sandbox-name length accepted by OpenShell 0.1.7.
pub(crate) const OPENSHELL_SANDBOX_NAME_LIMIT: usize = 19;

pub const WORKDIR_TARGET: &str = "/sandbox";
pub const CHECKOUT_TARGET: &str = "/sandbox/src";
pub const INSTRUCTIONS_TARGET: &str = "/sandbox/instructions";
pub const OUTPUT_TARGET: &str = "/sandbox/output";
pub const OUTPUT_FILE_NAME: &str = "review-output.json";
pub const OUTPUT_SCHEMA_FILE_NAME: &str = "review-output.schema.json";

pub fn system_prompt_file() -> String {
    format!("{INSTRUCTIONS_TARGET}/system-prompt.txt")
}

pub fn review_output_schema_file() -> String {
    format!("{INSTRUCTIONS_TARGET}/{OUTPUT_SCHEMA_FILE_NAME}")
}
