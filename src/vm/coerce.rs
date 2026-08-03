//! Coercing model replies and values into typed Jade values.
//!
//! `coerce` turns a raw string into an `int`/`float`/`bool`/`str` or a struct;
//! `vm_type_call` implements calling a type as a constructor (`City(dict)`,
//! `int("3")`). The struct-decorator application and the JSON→VmValue bridge a
//! coerced reply travels through live here too.

use super::*;

/// Resolve a possibly-dotted decorator name to a callable VmValue.
/// "tools.on_fail" → GetGlobal("tools") → GetMethod("on_fail") as BoundMethod.
/// Mirrors what the function-decorator emitter does with bytecode at compile time.
pub(crate) fn resolve_decorator_fn(dec_name: &str, state: &VmState) -> Option<VmValue> {
    if let Some(dot) = dec_name.find('.') {
        let base_name = &dec_name[..dot];
        let field_name = &dec_name[dot + 1..];
        match state.globals.get(base_name)?.clone() {
            VmValue::Struct(arc) => {
                let (type_name, field_val) = {
                    let guard = arc.lock();
                    (guard.type_name().to_string(), guard.get_field(field_name).cloned())
                };
                if let Some(v) = field_val {
                    return Some(v);
                }
                let mfn = state.extend_methods.get(&type_name)?.get(field_name)?.clone();
                Some(VmValue::BoundMethod(Arc::new(VmBoundMethod {
                    receiver: arc,
                    method: mfn,
                })))
            }
            VmValue::Dict(map) => map.get(field_name).cloned(),
            _ => None,
        }
    } else {
        state.globals.get(dec_name).cloned()
    }
}

/// Apply any struct decorators registered for `type_name` to a coerced value.
/// Called after every successful `coerce()` so decorator behaviour is identical
/// whether the struct came from a literal or from `?p |> Type`.
pub(crate) async fn apply_struct_decorators(
    mut v: VmValue,
    type_name: &str,
    state: &mut VmState,
    span: Span,
) -> Result<VmValue> {
    let decs = state.struct_decorators.get(type_name).cloned().unwrap_or_default();
    for (dec_name, dec_args) in decs {
        if let Some(dec_fn) = resolve_decorator_fn(&dec_name, state) {
            let mut call_args = vec![v];
            call_args.extend(dec_args);
            v = call_value(dec_fn, call_args, state, span).await?;
        }
    }
    Ok(v)
}


/// Recursively convert a `serde_json::Value` to a `VmValue`.
pub(crate) fn json_to_vm_value(json: &serde_json::Value) -> std::result::Result<VmValue, String> {
    match json {
        serde_json::Value::Null => Err("null is not a valid Jade value".to_string()),
        serde_json::Value::Bool(b) => Ok(VmValue::Bool(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() { Ok(VmValue::Int(i)) }
            else if let Some(f) = n.as_f64() { Ok(VmValue::Float(f)) }
            else { Err(format!("number {} cannot be represented as int or float", n)) }
        }
        serde_json::Value::String(s) => Ok(VmValue::Str(s.clone().into())),
        serde_json::Value::Array(arr) => arr.iter().enumerate()
            .map(|(i, v)| json_to_vm_value(v).map_err(|e| format!("element {}: {}", i, e)))
            .collect::<std::result::Result<Vec<VmValue>, String>>()
            .map(|v| VmValue::Array(Arc::new(Mutex::new(ArrayObj::from_vec(v))))),
        serde_json::Value::Object(obj) => obj.iter()
            .map(|(k, v)| json_to_vm_value(v)
                .map(|val| (k.clone(), val))
                .map_err(|e| format!("field '{}': {}", k, e)))
            .collect::<std::result::Result<DictObj<VmValue>, String>>()
            .map(VmValue::Dict),
    }
}

/// Summarise struct field names and optionality for LLM error messages.
pub(crate) fn vm_field_summary(def: &[StructFieldDef]) -> String {
    def.iter().map(|f| match f {
        StructFieldDef::Required(n)      => format!("{} (required)", n),
        StructFieldDef::Let { name, .. } => format!("{} (optional)", name),
        StructFieldDef::Prompt { name, .. } => format!("{} (prompt, optional)", name),
    }).collect::<Vec<_>>().join(", ")
}

/// Parse an LLM JSON response into a struct `VmValue`.
pub(crate) fn vm_coerce_struct(
    text: &str,
    type_name: &str,
    def: &[StructFieldDef],
) -> std::result::Result<VmValue, String> {
    use jade_runtime::coercef::{coerce_fields, FieldSpec};

    // Optional fields carry their declared default; a `Required` field carries
    // none, which is what makes it required. A `prompt` field is optional and
    // has no default to fall back on, so an omitted one stays absent — it is
    // left out of the spec entirely rather than being defaulted to nil.
    let specs: Vec<FieldSpec<VmValue>> = def
        .iter()
        .filter_map(|f| match f {
            StructFieldDef::Required(name) => {
                Some(FieldSpec { name: name.clone(), default: None })
            }
            StructFieldDef::Let { name, default } => Some(FieldSpec {
                name: name.clone(),
                default: Some(eval_literal_default(default).unwrap_or(VmValue::Nil)),
            }),
            StructFieldDef::Prompt { name, .. } => {
                if reply_has_field(text, name) {
                    Some(FieldSpec { name: name.clone(), default: None })
                } else {
                    None
                }
            }
        })
        .collect();

    // The rule — extracting JSON from prose, declaration order, defaults, which
    // failures re-prompt — is shared with the compiled path. The only thing
    // supplied here is how a JSON value becomes a `VmValue`.
    let is_prompt_field = |name: &str| {
        def.iter()
            .any(|f| matches!(f, StructFieldDef::Prompt { name: n, .. } if n == name))
    };
    let pairs = coerce_fields(text, &specs, |name, v| {
        if is_prompt_field(name) {
            return v
                .as_str()
                .map(|s| VmValue::Prompt(s.to_string()))
                .ok_or_else(|| format!("Prompt field '{name}' must be a string value."));
        }
        json_to_vm_value(v)
    })
    .map_err(|e| describe_coerce_error(e, type_name, def))?;

    let mut sobj = StructObj::<VmValue>::new(type_name);
    for (k, v) in pairs {
        sobj.set_field(&k, v);
    }
    Ok(VmValue::Struct(Arc::new(Mutex::new(sobj))))
}

/// Whether the reply carries `field` at all — used to keep an omitted `prompt`
/// field absent rather than giving it a value it never declared a default for.
pub(crate) fn reply_has_field(text: &str, field: &str) -> bool {
    let raw = jade_runtime::coercef::extract_json(text);
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v.as_object().map(|o| o.contains_key(field)))
        .unwrap_or(false)
}

/// Turn a coercion failure into the correction sent back to the model.
///
/// These strings are not user-facing diagnostics — they are the *next prompt*,
/// so each says what was wrong and what to send instead. That is why they live
/// here rather than in the shared rule: the compiled path re-prompts with its
/// own fixed wording and has no struct definition to summarise.
pub(crate) fn describe_coerce_error(
    e: jade_runtime::coercef::CoerceError,
    type_name: &str,
    def: &[StructFieldDef],
) -> String {
    use jade_runtime::coercef::CoerceError as E;
    match e {
        E::NotJson(detail) => format!(
            "Your response could not be parsed as a {} struct: {}. \
             Respond with a JSON object with fields: {}.",
            type_name, detail, vm_field_summary(def)
        ),
        E::NotObject => format!(
            "Your response is not a JSON object. \
             Respond with a JSON object for struct '{}' with fields: {}.",
            type_name, vm_field_summary(def)
        ),
        E::MissingRequired(name) => format!(
            "Missing required field '{}' for struct '{}'. \
             Respond with a JSON object containing all required fields: {}.",
            name, type_name, vm_field_summary(def)
        ),
        E::BadField { name, detail } => format!(
            "Field '{}' is invalid: {}. \
             Respond with a corrected JSON object for struct '{}'.",
            name, detail, type_name
        ),
    }
}

/// Call a type as a constructor: `int("3")`, `char("x")`, `str(v)`.
///
/// A *struct* type is deliberately not callable — see the arm at the bottom.
pub(crate) fn vm_type_call(
    type_name: String,
    arg: VmValue,
    state: &VmState,
    span: Span,
) -> Result<VmValue> {
    let err = |msg: String| Err(JadeError::Exception { message: msg, span });
    match type_name.as_str() {
        "int" => match arg {
            VmValue::Int(i)   => Ok(VmValue::Int(i)),
            VmValue::Float(f) => Ok(VmValue::Int(f as i64)),
            VmValue::Bool(b)  => Ok(VmValue::Int(if b { 1 } else { 0 })),
            VmValue::Str(s)   => s.trim().parse::<i64>()
                .map(VmValue::Int)
                .map_err(|_| JadeError::Exception {
                    message: format!("int(): cannot convert {:?} to int", s),
                    span,
                }),
            other => err(format!("int(): cannot convert {} to int", value_to_display(&other))),
        },
        "float" => match arg {
            VmValue::Float(f) => Ok(VmValue::Float(f)),
            VmValue::Int(i)   => Ok(VmValue::Float(i as f64)),
            VmValue::Bool(b)  => Ok(VmValue::Float(if b { 1.0 } else { 0.0 })),
            VmValue::Str(s)   => s.trim().parse::<f64>()
                .map(VmValue::Float)
                .map_err(|_| JadeError::Exception {
                    message: format!("float(): cannot convert {:?} to float", s),
                    span,
                }),
            other => err(format!("float(): cannot convert {} to float", value_to_display(&other))),
        },
        "bool" => match arg {
            VmValue::Bool(b)  => Ok(VmValue::Bool(b)),
            VmValue::Int(i)   => Ok(VmValue::Bool(i != 0)),
            VmValue::Float(f) => Ok(VmValue::Bool(f != 0.0)),
            VmValue::Nil      => Ok(VmValue::Bool(false)),
            VmValue::Str(s)   => match s.to_lowercase().as_str() {
                "true"  => Ok(VmValue::Bool(true)),
                "false" => Ok(VmValue::Bool(false)),
                ""      => Ok(VmValue::Bool(false)),
                _       => Ok(VmValue::Bool(true)),
            },
            other => Ok(VmValue::Bool(!matches!(other, VmValue::Nil))),
        },
        "str" => {
            // Rendering a tainted string does not make it trustworthy.
            let trust = match &arg {
                VmValue::Str(s) => s.trust(),
                VmValue::Char(c) => c.trust(),
                _ => jade_runtime::trust::TRUSTED,
            };
            Ok(VmValue::Str(JStr::with_trust(value_to_display(&arg), trust)))
        }
        "char" => match arg {
            VmValue::Char(c) => Ok(VmValue::Char(c)),
            // Exactly one character, so the conversion cannot silently drop
            // input. A multi-character string is a mistake worth reporting.
            VmValue::Str(s) => {
                let mut it = s.chars();
                match (it.next(), it.next()) {
                    (Some(c), None) => Ok(VmValue::Char(
                        jade_runtime::trust::JChar::with_trust(c, s.trust()),
                    )),
                    _ => err(format!(
                        "char(): expected a string of exactly one character, got {:?}",
                        s.as_str()
                    )),
                }
            }
            other => err(format!("char(): cannot convert {} to char", value_to_display(&other))),
        },
        "func" => match arg {
            VmValue::Str(name) => state.globals.get(name.as_str()).cloned().ok_or_else(|| {
                JadeError::Exception {
                    message: format!("func(): no function named {:?}", name),
                    span,
                }
            }),
            other if matches!(
                other,
                VmValue::Fn(_) | VmValue::Closure(_, _) | VmValue::BoundMethod(_) | VmValue::BuiltinFn(_)
            ) => Ok(other),
            other => err(format!("func(): expected a string or function, got {}", value_to_display(&other))),
        },
        // A struct type is not callable. `City { name: "x" }` is the one way to
        // build a struct, and it is the only one that checks required fields
        // and applies declared defaults.
        //
        // Calling the type used to work, as an undeclared second construction
        // path that did neither: it filled every missing field with nil, so a
        // required field could be silently absent and `let population = 0` was
        // ignored. Nobody declared that behaviour; it fell out of type names
        // being callable values. A conversion is an ordinary function.
        name => {
            let _ = &arg;
            if state.struct_defs.contains_key(name) {
                err(format!(
                    "{}(): a struct type is not a function — build one with {} {{ ... }}",
                    name, name
                ))
            } else {
                err(format!("{}(): unknown type", name))
            }
        }
    }
}

/// Try to coerce a raw LLM response to a `VmValue`.
/// Returns `Ok(value)` on success or `Err(correction_prompt)` on failure —
/// the correction is fed back to the LLM, never surfaced to the user directly.
pub(crate) fn coerce(
    text: &str,
    type_name: &str,
    struct_defs: &HashMap<String, Vec<StructFieldDef>>,
) -> std::result::Result<VmValue, String> {
    match type_name {
        "int" => text.parse::<i64>().map(VmValue::Int).map_err(|_| format!(
            "Your response {:?} could not be parsed as an integer. \
             Respond with only a plain integer, e.g. 42.",
            text
        )),
        "float" => text.parse::<f64>().map(VmValue::Float).map_err(|_| format!(
            "Your response {:?} could not be parsed as a float. \
             Respond with only a plain float, e.g. 3.14.",
            text
        )),
        "str" => Ok(VmValue::Str(text.to_string().into())),
        "char" => {
            let mut it = text.chars();
            match (it.next(), it.next()) {
                (Some(c), None) => Ok(VmValue::Char(jade_runtime::trust::JChar::trusted(c))),
                _ => Err(format!(
                    "Your response {:?} was not a single character. \
                     Respond with exactly one character.",
                    text
                )),
            }
        }
        "bool" => match text.to_lowercase().as_str() {
            "true"  => Ok(VmValue::Bool(true)),
            "false" => Ok(VmValue::Bool(false)),
            _ => Err(format!(
                "Your response {:?} could not be parsed as a boolean. \
                 Respond with only 'true' or 'false'.",
                text
            )),
        },
        "Array" | "array" => {
            let raw = jade_runtime::coercef::extract_json(text);
            serde_json::from_str::<serde_json::Value>(&raw)
                .map_err(|e| format!(
                    "Your response could not be parsed as a JSON array: {}. \
                     Respond with only a JSON array, e.g. [1, \"two\", true].",
                    e
                ))
                .and_then(|v| match v {
                    serde_json::Value::Array(arr) => arr.iter().enumerate()
                        .map(|(i, elem)| json_to_vm_value(elem)
                            .map_err(|e| format!("element {}: {}", i, e)))
                        .collect::<std::result::Result<Vec<VmValue>, String>>()
                        .map(|v| VmValue::Array(Arc::new(Mutex::new(ArrayObj::from_vec(v)))))
                        .map_err(|e| format!(
                            "Your response array could not be fully converted: {}. \
                             Respond with only a JSON array of int, float, bool, or string values.",
                            e
                        )),
                    _ => Err("Your response is not a JSON array. \
                              Respond with only a JSON array, e.g. [1, \"two\", true].".to_string()),
                })
        }
        "Dict" | "dict" => {
            let raw = jade_runtime::coercef::extract_json(text);
            serde_json::from_str::<serde_json::Value>(&raw)
                .map_err(|e| format!(
                    "Your response could not be parsed as a JSON object: {}. \
                     Respond with only a JSON object, e.g. {{\"key\": \"value\"}}.",
                    e
                ))
                .and_then(|v| match v {
                    serde_json::Value::Object(obj) => obj.iter()
                        .map(|(k, val)| json_to_vm_value(val)
                            .map(|v| (k.clone(), v))
                            .map_err(|e| format!("field '{}': {}", k, e)))
                        .collect::<std::result::Result<DictObj<VmValue>, String>>()
                        .map(VmValue::Dict)
                        .map_err(|e| format!(
                            "Your response dict could not be fully converted: {}. \
                             Respond with only a JSON object, e.g. {{\"key\": \"value\"}}.",
                            e
                        )),
                    _ => Err("Your response is not a JSON object. \
                              Respond with only a JSON object, e.g. {\"key\": \"value\"}.".to_string()),
                })
        }
        name => {
            if let Some(def) = struct_defs.get(name) {
                vm_coerce_struct(text, name, def)
            } else {
                Err(format!(
                    "Unknown type '{}'. Cannot coerce LLM response to this type.", name
                ))
            }
        }
    }
}
