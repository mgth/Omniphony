//! Declarative parameter schema, shared by every plugin.
//!
//! A plugin — a render backend, an object generator, the phantom-extraction
//! stage — describes its tunable parameters as data
//! ([`PluginFactory::param_schema`](crate::plugin::PluginFactory::param_schema)),
//! so the UI can render controls and the host can store/transport values
//! generically — no per-plugin typed field in the renderer core, and no
//! hand-written serde bridge. Values live in one generic
//! `plugin id -> key -> ParamValue` store per plugin kind
//! ([`crate::plugin::PluginParams`], held by `RendererControl`); a backend reads
//! them at **build time** via
//! [`BackendBuildCtx::backend_param`](crate::backend_registry::BackendBuildCtx::backend_param),
//! a synthesizing stage when they change — never on the audio hot path.

/// A single tunable parameter exposed by a plugin.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParamSpec {
    /// Stable key used to store/transport the value (e.g. `"sharpness"`).
    pub key: &'static str,
    /// Human-facing label for the UI: the English fallback when `i18n_key`
    /// has no translation.
    pub label: &'static str,
    /// Key of a localized label in Studio's catalogues (built-in plugins);
    /// `None` for an out-of-tree plugin, whose `label` is shown as is.
    #[serde(rename = "i18nKey", skip_serializing_if = "Option::is_none")]
    pub i18n_key: Option<&'static str>,
    /// Display unit suffix of a numeric value (e.g. `"Hz"`, `"dB"`). `None`
    /// for a bare number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<&'static str>,
    /// Control type and its bounds.
    pub kind: ParamKind,
    /// Value used when the host has not set one. The backend applies the same
    /// default in code; the schema mirrors it so the UI can show it.
    pub default: ParamValue,
    /// Optional capability that gates this control's visibility (e.g. only show
    /// when the backend `supports_distance_model`). `None` = always shown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires: Option<&'static str>,
    /// Optional one-line help shown next to the control (e.g. as an info tooltip).
    /// `None` = no help affordance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<&'static str>,
}

impl ParamSpec {
    /// Float control with inclusive `[min, max]` bounds and a UI step.
    pub fn float(
        key: &'static str,
        label: &'static str,
        min: f32,
        max: f32,
        step: f32,
        default: f32,
    ) -> Self {
        Self {
            key,
            label,
            i18n_key: None,
            unit: None,
            kind: ParamKind::Float { min, max, step },
            default: ParamValue::Float(default),
            requires: None,
            help: None,
        }
    }

    /// Integer control with inclusive `[min, max]` bounds.
    pub fn int(key: &'static str, label: &'static str, min: i64, max: i64, default: i64) -> Self {
        Self {
            key,
            label,
            i18n_key: None,
            unit: None,
            kind: ParamKind::Int { min, max },
            default: ParamValue::Int(default),
            requires: None,
            help: None,
        }
    }

    /// Boolean (checkbox) control.
    pub fn bool(key: &'static str, label: &'static str, default: bool) -> Self {
        Self {
            key,
            label,
            i18n_key: None,
            unit: None,
            kind: ParamKind::Bool,
            default: ParamValue::Bool(default),
            requires: None,
            help: None,
        }
    }

    /// Filesystem-path control: a text field plus a file picker in the UI. The
    /// value is carried as [`ParamValue::Text`]. Used by backends that load an
    /// external asset (e.g. the scriptable backend's `.lua` file).
    pub fn path(key: &'static str, label: &'static str, default: &str) -> Self {
        Self {
            key,
            label,
            i18n_key: None,
            unit: None,
            kind: ParamKind::Path,
            default: ParamValue::Text(default.to_string()),
            requires: None,
            help: None,
        }
    }

    /// Editable-file control: a handle field plus a Browse button and, when
    /// `editable`, an Edit button opening a local editor. See [`ParamKind::File`].
    /// The value is carried as a [`ParamValue::Text`] handle whose *content* is
    /// owned by the renderer (so it works with a remote renderer); `language` is a
    /// UI hint (e.g. `Some("lua")`) and `extensions` restricts the Browse picker.
    pub fn file(
        key: &'static str,
        label: &'static str,
        default: &str,
        editable: bool,
        language: Option<&'static str>,
        extensions: Vec<&'static str>,
    ) -> Self {
        Self {
            key,
            label,
            i18n_key: None,
            unit: None,
            kind: ParamKind::File {
                editable,
                language,
                extensions,
            },
            default: ParamValue::Text(default.to_string()),
            requires: None,
            help: None,
        }
    }

    /// Gate this control behind a capability flag of its plugin (a backend
    /// capability, or the phantom extractor's active method).
    pub fn requires(mut self, capability: &'static str) -> Self {
        self.requires = Some(capability);
        self
    }

    /// Attach a one-line help string, shown next to the control in the UI.
    pub fn help(mut self, help: &'static str) -> Self {
        self.help = Some(help);
        self
    }

    /// Attach the key of a localized label in Studio's catalogues.
    pub fn i18n(mut self, key: &'static str) -> Self {
        self.i18n_key = Some(key);
        self
    }

    /// Attach a display unit suffix (e.g. `"Hz"`).
    pub fn unit(mut self, unit: &'static str) -> Self {
        self.unit = Some(unit);
        self
    }

    /// `value` in the type this parameter declares, or `None` when it cannot
    /// be read as one: a number for a bool is on at `>= 0.5` (how the float-only
    /// parameters of older clients and configs spelled a switch), a float for an
    /// int is rounded, an enum value must be one of the options. Types only —
    /// bounds are the plugin's to clamp, as it always did.
    pub fn coerce(&self, value: &ParamValue) -> Option<ParamValue> {
        match &self.kind {
            ParamKind::Float { .. } => value
                .as_f32()
                .filter(|v| v.is_finite())
                .map(ParamValue::Float),
            ParamKind::Int { .. } => match value {
                ParamValue::Int(v) => Some(ParamValue::Int(*v)),
                ParamValue::Float(v) if v.is_finite() => Some(ParamValue::Int(v.round() as i64)),
                _ => None,
            },
            ParamKind::Bool => value.as_switch().map(ParamValue::Bool),
            ParamKind::Enum { options } => value
                .as_str()
                .filter(|v| options.iter().any(|o| o.value == *v))
                .map(|v| ParamValue::Text(v.to_string())),
            ParamKind::Path | ParamKind::File { .. } => {
                value.as_str().map(|v| ParamValue::Text(v.to_string()))
            }
        }
    }
}

/// Control type and bounds for a [`ParamSpec`]. Tagged so the UI gets a discriminator.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParamKind {
    Float {
        min: f32,
        max: f32,
        step: f32,
    },
    Int {
        min: i64,
        max: i64,
    },
    Bool,
    Enum {
        options: Vec<ParamOption>,
    },
    /// A filesystem path: rendered as a text field with a file picker. Value is
    /// a [`ParamValue::Text`].
    Path,
    /// An editable file resource whose content is owned by the renderer. Rendered
    /// as a handle field with a Browse button (offered only when the renderer is
    /// local) and, when `editable`, an Edit button opening a local editor. The
    /// value is a [`ParamValue::Text`] handle — an absolute path on the renderer
    /// host, or a bare name in the renderer's managed store. Its *content* is
    /// fetched/pushed over OSC, so it works with a remote renderer and no shared
    /// filesystem.
    File {
        /// Whether the UI offers an editor for this file.
        editable: bool,
        /// Optional editor language hint (e.g. `"lua"`).
        #[serde(skip_serializing_if = "Option::is_none")]
        language: Option<&'static str>,
        /// Extensions the Browse picker restricts to (e.g. `["lua"]`).
        extensions: Vec<&'static str>,
    },
}

/// One choice of an [`ParamKind::Enum`] control.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParamOption {
    pub value: String,
    pub label: String,
}

/// A concrete parameter value. Untagged so it serialises to a bare JSON scalar
/// (`1.5`, `true`, `"x"`) for the UI.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ParamValue {
    Bool(bool),
    Int(i64),
    Float(f32),
    Text(String),
}

impl ParamValue {
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            ParamValue::Float(v) => Some(*v),
            ParamValue::Int(v) => Some(*v as f32),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            ParamValue::Int(v) => Some(*v),
            ParamValue::Float(v) => Some(*v as i64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            ParamValue::Bool(v) => Some(*v),
            _ => None,
        }
    }

    /// A switch read from a bool or from a number (on at `>= 0.5`): how the
    /// parameters that were float-only spelled one, so a value an older
    /// client or config wrote still reads.
    pub fn as_switch(&self) -> Option<bool> {
        match self {
            ParamValue::Bool(v) => Some(*v),
            ParamValue::Float(v) if v.is_finite() => Some(*v >= 0.5),
            ParamValue::Int(v) => Some(*v as f32 >= 0.5),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            ParamValue::Text(v) => Some(v),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_accessors_and_numeric_coercion() {
        assert_eq!(ParamValue::Float(1.5).as_f32(), Some(1.5));
        // Int coerces to f32 and vice versa, so a UI sending an integer for a
        // float param still resolves.
        assert_eq!(ParamValue::Int(3).as_f32(), Some(3.0));
        assert_eq!(ParamValue::Float(2.9).as_i64(), Some(2));
        assert_eq!(ParamValue::Bool(true).as_f32(), None);
        assert_eq!(ParamValue::Bool(true).as_bool(), Some(true));
        assert_eq!(ParamValue::Text("x".into()).as_str(), Some("x"));
    }

    #[test]
    fn float_value_serialises_to_a_bare_scalar() {
        // Untagged: the UI receives `2.0`, not `{"Float":2.0}`.
        let json = serde_json::to_string(&ParamValue::Float(2.0)).unwrap();
        assert_eq!(json, "2.0");
    }

    #[test]
    fn file_kind_serialises_with_a_type_discriminator() {
        // The UI switches on `kind.type`; File must tag as "file" and carry its
        // editor hints. `language: None` is skipped.
        let spec = ParamSpec::file("path", "Script file", "", true, Some("lua"), vec!["lua"]);
        assert!(matches!(spec.kind, ParamKind::File { editable: true, .. }));
        let json = serde_json::to_string(&spec.kind).unwrap();
        assert_eq!(
            json,
            r#"{"type":"file","editable":true,"language":"lua","extensions":["lua"]}"#
        );
        // Default value is a bare text handle.
        assert_eq!(spec.default.as_str(), Some(""));
    }

    #[test]
    fn coerce_reads_a_value_in_the_declared_type() {
        let float = ParamSpec::float("f", "F", 0.0, 1.0, 0.01, 0.5);
        assert_eq!(
            float.coerce(&ParamValue::Int(1)),
            Some(ParamValue::Float(1.0))
        );
        assert_eq!(float.coerce(&ParamValue::Float(f32::NAN)), None);
        assert_eq!(float.coerce(&ParamValue::Bool(true)), None);
        let int = ParamSpec::int("i", "I", 1, 3, 1);
        assert_eq!(
            int.coerce(&ParamValue::Float(2.6)),
            Some(ParamValue::Int(3))
        );
        // A switch spelled as a number by a float-only client or config.
        let switch = ParamSpec::bool("b", "B", false);
        assert_eq!(
            switch.coerce(&ParamValue::Float(1.0)),
            Some(ParamValue::Bool(true))
        );
        assert_eq!(
            switch.coerce(&ParamValue::Float(0.0)),
            Some(ParamValue::Bool(false))
        );
        assert_eq!(
            switch.coerce(&ParamValue::Int(1)),
            Some(ParamValue::Bool(true))
        );
        assert_eq!(switch.coerce(&ParamValue::Text("x".into())), None);
        let choice = ParamSpec {
            kind: ParamKind::Enum {
                options: vec![ParamOption {
                    value: "a".into(),
                    label: "A".into(),
                }],
            },
            ..ParamSpec::path("e", "E", "a")
        };
        assert_eq!(
            choice.coerce(&ParamValue::Text("a".into())),
            Some(ParamValue::Text("a".into()))
        );
        assert_eq!(choice.coerce(&ParamValue::Text("b".into())), None);
    }

    #[test]
    fn i18n_key_and_unit_serialise_only_when_set() {
        let bare = serde_json::to_value(ParamSpec::float("f", "F", 0.0, 1.0, 0.1, 0.5)).unwrap();
        assert!(bare.get("i18nKey").is_none() && bare.get("unit").is_none());
        let spec = ParamSpec::float("hpf_hz", "Cutoff", 20.0, 2000.0, 10.0, 300.0)
            .i18n("twoDSources.padHpf")
            .unit("Hz");
        let json = serde_json::to_value(spec).unwrap();
        assert_eq!(json["i18nKey"], "twoDSources.padHpf");
        assert_eq!(json["unit"], "Hz");
    }

    #[test]
    fn spec_builders_set_kind_and_default() {
        let spec = ParamSpec::float("sharpness", "Sharpness", 0.5, 8.0, 0.1, 2.0);
        assert_eq!(spec.key, "sharpness");
        assert!(matches!(spec.kind, ParamKind::Float { .. }));
        assert_eq!(spec.default.as_f32(), Some(2.0));
        assert!(spec.requires.is_none());
        assert_eq!(
            ParamSpec::bool("x", "X", true)
                .requires("supports_x")
                .requires,
            Some("supports_x")
        );
    }
}
