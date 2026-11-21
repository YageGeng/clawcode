//! MCP elicitation forms: the fields a requested schema asks for, and the
//! validation that turns typed values into the accepted content.
//!
//! Mirrors `web/src/features/mcp/elicitationForm.ts`.

use std::collections::BTreeMap;

use agent_client_protocol::v2::schema as wire;

/// What kind of input a field takes.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    /// Free text with optional length bounds.
    Text {
        /// Minimum length in code points.
        min: Option<u32>,
        /// Maximum length in code points.
        max: Option<u32>,
    },
    /// One of several `(value, label)` options.
    Choice {
        /// Offered options.
        options: Vec<(String, String)>,
    },
    /// A number, optionally integral, with optional bounds.
    Number {
        /// Whether only whole numbers are accepted.
        integer: bool,
        /// Inclusive minimum.
        min: Option<f64>,
        /// Inclusive maximum.
        max: Option<f64>,
    },
    /// A yes/no checkbox.
    Bool,
    /// Several of the `(value, label)` options.
    MultiChoice {
        /// Offered options.
        options: Vec<(String, String)>,
        /// Fewest picks.
        min_items: Option<u64>,
        /// Most picks.
        max_items: Option<u64>,
    },
}

/// One field of the form.
#[derive(Debug, Clone, PartialEq, typed_builder::TypedBuilder)]
pub struct FormField {
    /// Property name sent back.
    pub name: String,
    /// Shown label: the schema title, else the name.
    pub label: String,
    /// Optional help text.
    #[builder(default)]
    pub description: Option<String>,
    /// Whether an answer is required.
    pub required: bool,
    /// Input kind.
    pub kind: FieldKind,
}

/// A value the user entered for one field.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    /// Text, including numbers as typed and the chosen option value.
    Text(String),
    /// A checkbox.
    Bool(bool),
    /// Several picked option values.
    Many(Vec<String>),
}

/// Why a field's value was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldError {
    /// A required field is empty.
    Required,
    /// Shorter than this many code points.
    TooShort(u32),
    /// Longer than this many code points.
    TooLong(u32),
    /// Not a number, not a whole number, or out of bounds.
    Number,
    /// Too few or too many picks.
    Count,
}

/// The schema of a request could not be shown: a required field has no usable input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unrenderable;

/// The fields of one elicitation form, in schema order.
#[derive(Debug, Clone, PartialEq)]
pub struct ElicitationForm {
    fields: Vec<FormField>,
}

impl TryFrom<&wire::ElicitationSchema> for ElicitationForm {
    type Error = Unrenderable;

    /// Keeps the fields this client can render; a required one it cannot render fails the form.
    fn try_from(
        schema: &wire::ElicitationSchema,
    ) -> Result<Self, Unrenderable> {
        let required = schema.required.clone().unwrap_or_default();
        // A required name without a property has nothing to render.
        if required
            .iter()
            .any(|name| !schema.properties.contains_key(name))
        {
            return Err(Unrenderable);
        }
        let mut fields = Vec::new();
        for (name, property) in &schema.properties {
            let is_required = required.contains(name);
            match FieldShape::from(property).0 {
                Some((title, description, kind)) => fields.push(
                    FormField::builder()
                        .name(name.clone())
                        .label(title.unwrap_or_else(|| name.clone()))
                        .description(description)
                        .required(is_required)
                        .kind(kind)
                        .build(),
                ),
                None if is_required => return Err(Unrenderable),
                None => {}
            }
        }
        Ok(ElicitationForm { fields })
    }
}

/// Title, description, and input kind of one property; `None` when unrenderable.
struct FieldShape(Option<(Option<String>, Option<String>, FieldKind)>);

impl From<&wire::ElicitationPropertySchema> for FieldShape {
    /// Maps each schema type to its input; unknown types are unrenderable.
    fn from(property: &wire::ElicitationPropertySchema) -> Self {
        let titled = |options: &[wire::EnumOption]| {
            options
                .iter()
                .map(|option| (option.value.clone(), option.title.clone()))
                .collect::<Vec<_>>()
        };
        FieldShape(match property {
            wire::ElicitationPropertySchema::String(string) => {
                let kind = match (&string.enum_values, &string.one_of) {
                    (Some(_), Some(_)) => return FieldShape(None),
                    (Some(values), None) => FieldKind::Choice {
                        options: values
                            .iter()
                            .map(|value| (value.clone(), value.clone()))
                            .collect(),
                    },
                    (None, Some(options)) => FieldKind::Choice {
                        options: titled(options),
                    },
                    (None, None) => FieldKind::Text {
                        min: string.min_length,
                        max: string.max_length,
                    },
                };
                Some((string.title.clone(), string.description.clone(), kind))
            }
            wire::ElicitationPropertySchema::Number(number) => Some((
                number.title.clone(),
                number.description.clone(),
                FieldKind::Number {
                    integer: false,
                    min: number.minimum,
                    max: number.maximum,
                },
            )),
            wire::ElicitationPropertySchema::Integer(integer) => Some((
                integer.title.clone(),
                integer.description.clone(),
                FieldKind::Number {
                    integer: true,
                    // Bounds are compared as floats; schema bounds are small in practice.
                    min: integer.minimum.map(|value| value as f64),
                    max: integer.maximum.map(|value| value as f64),
                },
            )),
            wire::ElicitationPropertySchema::Boolean(boolean) => Some((
                boolean.title.clone(),
                boolean.description.clone(),
                FieldKind::Bool,
            )),
            wire::ElicitationPropertySchema::Array(array) => {
                let options = match &array.items {
                    wire::MultiSelectItems::String(items) => items
                        .values
                        .iter()
                        .map(|value| (value.clone(), value.clone()))
                        .collect(),
                    wire::MultiSelectItems::Titled(items) => {
                        titled(&items.options)
                    }
                    _ => return FieldShape(None),
                };
                Some((
                    array.title.clone(),
                    array.description.clone(),
                    FieldKind::MultiChoice {
                        options,
                        min_items: array.min_items,
                        max_items: array.max_items,
                    },
                ))
            }
            _ => None,
        })
    }
}

impl ElicitationForm {
    /// The fields to show, in schema order.
    pub fn fields(&self) -> &[FormField] {
        &self.fields
    }

    /// Checks every value and builds the accepted content; empty optional fields are left out.
    pub fn validate(
        &self,
        values: &BTreeMap<String, FieldValue>,
    ) -> Result<
        BTreeMap<String, wire::ElicitationContentValue>,
        BTreeMap<String, FieldError>,
    > {
        let mut content = BTreeMap::new();
        let mut errors = BTreeMap::new();
        for field in &self.fields {
            match field.check(values.get(&field.name)) {
                Ok(Some(value)) => {
                    content.insert(field.name.clone(), value);
                }
                Ok(None) => {}
                Err(error) => {
                    errors.insert(field.name.clone(), error);
                }
            }
        }
        if errors.is_empty() {
            Ok(content)
        } else {
            Err(errors)
        }
    }
}

impl FormField {
    /// Checks one value; `Ok(None)` is an empty optional field.
    fn check(
        &self,
        value: Option<&FieldValue>,
    ) -> Result<Option<wire::ElicitationContentValue>, FieldError> {
        let text = match value {
            Some(FieldValue::Text(text)) => text.trim(),
            _ => "",
        };
        match &self.kind {
            FieldKind::Bool => {
                Ok(Some(wire::ElicitationContentValue::Boolean(matches!(
                    value,
                    Some(FieldValue::Bool(true))
                ))))
            }
            FieldKind::MultiChoice {
                min_items,
                max_items,
                ..
            } => {
                let picks = match value {
                    Some(FieldValue::Many(picks)) => picks.clone(),
                    _ => Vec::new(),
                };
                if picks.is_empty() {
                    return if self.required {
                        Err(FieldError::Required)
                    } else {
                        Ok(None)
                    };
                }
                let count = u64::try_from(picks.len()).unwrap_or(u64::MAX);
                if min_items.is_some_and(|min| count < min)
                    || max_items.is_some_and(|max| count > max)
                {
                    return Err(FieldError::Count);
                }
                Ok(Some(wire::ElicitationContentValue::StringArray(picks)))
            }
            _ if text.is_empty() => {
                if self.required {
                    Err(FieldError::Required)
                } else {
                    Ok(None)
                }
            }
            FieldKind::Text { min, max } => {
                // Lengths count code points, like the WebUI.
                let length =
                    u32::try_from(text.chars().count()).unwrap_or(u32::MAX);
                if let Some(min) = *min
                    && length < min
                {
                    return Err(FieldError::TooShort(min));
                }
                if let Some(max) = *max
                    && length > max
                {
                    return Err(FieldError::TooLong(max));
                }
                Ok(Some(wire::ElicitationContentValue::String(text.to_owned())))
            }
            FieldKind::Choice { .. } => {
                Ok(Some(wire::ElicitationContentValue::String(text.to_owned())))
            }
            FieldKind::Number { integer, min, max } => {
                let number: f64 =
                    text.parse().map_err(|_bad| FieldError::Number)?;
                // `NaN` and `inf` parse as floats but are not JSON numbers.
                if !number.is_finite()
                    || min.is_some_and(|min| number < min)
                    || max.is_some_and(|max| number > max)
                {
                    return Err(FieldError::Number);
                }
                if *integer {
                    let whole: i64 =
                        text.parse().map_err(|_bad| FieldError::Number)?;
                    Ok(Some(wire::ElicitationContentValue::Integer(whole)))
                } else {
                    Ok(Some(wire::ElicitationContentValue::Number(number)))
                }
            }
        }
    }
}
