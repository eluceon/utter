pub const SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub text: String,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TranscribeOptions {
    pub language: Option<String>,       // ISO 639-1; None = auto-detect
    pub initial_prompt: Option<String>, // dictionary terms hint
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Verbatim,
    Clean,
    Formal,
    Notes,
    CodeComment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionMethod {
    ClipboardPaste,
    Type,
    ClipboardOnly,
}
