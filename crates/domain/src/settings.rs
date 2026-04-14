//! Single-restaurant configuration persisted by the storage layer.
//!
//! One row (singleton) holds JSON; defaults match the previous hard-coded agent demo so tests
//! and fresh installs behave consistently.

use serde::{Deserialize, Serialize};

/// Restaurant profile and voice used by the reply agent and web UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestaurantSettings {
    pub restaurant_name: String,
    /// Short cuisine / concept line (e.g. "Italian trattoria").
    pub cuisine_style: String,
    /// Free-form context for the LLM (location, ethos, policies in prose).
    pub context_line: String,
    /// How replies should sound (warm, formal, brief, …).
    pub voice_tone: Option<String>,
    pub signature_dishes: Vec<String>,
    /// Human-readable hours blurb for prompts and UI.
    pub opening_hours_text: Option<String>,
    /// Optional quiet-hours note for notifications (full scheduling is notifier-side).
    pub notifier_quiet_hours: Option<String>,
}

impl Default for RestaurantSettings {
    fn default() -> Self {
        Self {
            restaurant_name: "Chez Luca".into(),
            cuisine_style: "Italian".into(),
            context_line: "A small family-run Italian trattoria in the neighbourhood.".into(),
            voice_tone: Some("warm, concise, grateful".into()),
            signature_dishes: vec!["Tagliatelle al ragù".into(), "Margherita pizza".into()],
            opening_hours_text: Some("Tue–Sun 17:00–22:00; closed Mondays.".into()),
            notifier_quiet_hours: None,
        }
    }
}

impl RestaurantSettings {
    /// Merge JSON from storage; unknown fields are ignored, missing keys use defaults.
    #[must_use]
    pub fn from_json_partial(value: &serde_json::Value) -> Self {
        serde_json::from_value(value.clone()).unwrap_or_else(|_| Self::default())
    }

    /// Apply a partial update (typical PUT body).
    #[must_use]
    pub fn merge(self, patch: RestaurantSettingsPatch) -> Self {
        let mut s = self;
        if let Some(v) = patch.restaurant_name {
            s.restaurant_name = v;
        }
        if let Some(v) = patch.cuisine_style {
            s.cuisine_style = v;
        }
        if let Some(v) = patch.context_line {
            s.context_line = v;
        }
        if let Some(v) = patch.voice_tone {
            s.voice_tone = v;
        }
        if let Some(v) = patch.signature_dishes {
            s.signature_dishes = v;
        }
        if let Some(v) = patch.opening_hours_text {
            s.opening_hours_text = v;
        }
        if let Some(v) = patch.notifier_quiet_hours {
            s.notifier_quiet_hours = v;
        }
        s
    }
}

/// Partial update for `PUT /api/v1/settings` (all fields optional).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RestaurantSettingsPatch {
    pub restaurant_name: Option<String>,
    pub cuisine_style: Option<String>,
    pub context_line: Option<String>,
    /// `null` clears the field in storage.
    pub voice_tone: Option<Option<String>>,
    pub signature_dishes: Option<Vec<String>>,
    pub opening_hours_text: Option<Option<String>>,
    pub notifier_quiet_hours: Option<Option<String>>,
}
