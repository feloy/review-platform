//! Fixed paths shared by OpenShell mounts and the review agent command.

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
