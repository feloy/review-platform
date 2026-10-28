//! Fixed paths shared by OpenShell mounts and the review agent command.

pub const CHECKOUT_TARGET: &str = "/sandbox/src";
pub const INSTRUCTIONS_TARGET: &str = "/sandbox/instructions";
pub const OUTPUT_TARGET: &str = "/sandbox/output";

pub fn system_prompt_file() -> String {
    format!("{INSTRUCTIONS_TARGET}/system-prompt.txt")
}

pub fn review_output_file() -> String {
    format!("{OUTPUT_TARGET}/review-output.json")
}
