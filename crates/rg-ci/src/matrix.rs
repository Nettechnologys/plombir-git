//! `strategy.matrix` of an Actions job: what it spells, and the variants it
//! expands to.
//!
//! A matrix is a mapping of dimensions plus GitHub's two adjustments,
//! `exclude` and `include`, and any part of it — or the whole of it — may be
//! `${{ fromJSON(needs.<job>.outputs.<name>) }}`. A matrix that reads a needed
//! job's outputs cannot be expanded when the workflow is read: the pipeline
//! graph carries the job as one placeholder, and the variants are written when
//! the stage it is in is reached (see `expand_deferred_matrix` in `lib.rs`).
//! Both paths expand with [`combinations`], so a matrix means the same thing
//! whichever of them it took.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// GitHub's cap on the variants one matrix job expands to.
pub(crate) const MAX_MATRIX_VARIANTS: usize = 256;

/// `needs.<job>.outputs.<output>`, as `fromJSON(…)` reads it. `job` is the id
/// the needing job's own file uses, which `needs_aliases` maps to the job in
/// the pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NeedsOutput {
    pub(crate) job: String,
    pub(crate) output: String,
}

impl std::fmt::Display for NeedsOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "needs.{}.outputs.{}", self.job, self.output)
    }
}

/// One entry of a matrix mapping — a dimension, `include` or `exclude`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MatrixEntry {
    /// Written in the file.
    Value(Value),
    /// `${{ fromJSON(needs.<job>.outputs.<output>) }}`.
    FromNeeds(NeedsOutput),
}

/// A job's `strategy.matrix` as the file spells it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MatrixSpec {
    /// `matrix: ${{ fromJSON(needs.<job>.outputs.<output>) }}`.
    Whole(NeedsOutput),
    /// A mapping, by key.
    Entries(BTreeMap<String, MatrixEntry>),
}

impl MatrixSpec {
    /// Read `strategy.matrix`. `Err` is the reason, for the caller to put
    /// behind the job and the path.
    pub(crate) fn parse(raw: &Value) -> Result<Self, (String, String)> {
        match raw {
            Value::String(text) => from_json_expression(text)
                .map(Self::Whole)
                .ok_or_else(|| (String::new(), from_json_refusal(text))),
            Value::Object(entries) => {
                let mut parsed = BTreeMap::new();
                for (key, value) in entries {
                    let entry = match value {
                        Value::String(text) => MatrixEntry::FromNeeds(
                            from_json_expression(text)
                                .ok_or_else(|| (format!(".{key}"), from_json_refusal(text)))?,
                        ),
                        value => {
                            check_entry(key, value)
                                .map_err(|(at, reason)| (format!(".{key}{at}"), reason))?;
                            MatrixEntry::Value(value.clone())
                        }
                    };
                    parsed.insert(key.clone(), entry);
                }
                Ok(Self::Entries(parsed))
            }
            _ => Err((
                String::new(),
                "must be a mapping, or `${{ fromJSON(needs.<job>.outputs.<name>) }}`".into(),
            )),
        }
    }

    /// The needed jobs' outputs this matrix reads; empty when it is all in
    /// the file.
    pub(crate) fn needs_outputs(&self) -> Vec<&NeedsOutput> {
        match self {
            Self::Whole(reference) => vec![reference],
            Self::Entries(entries) => entries
                .values()
                .filter_map(|entry| match entry {
                    MatrixEntry::FromNeeds(reference) => Some(reference),
                    MatrixEntry::Value(_) => None,
                })
                .collect(),
        }
    }

    /// The matrix as one JSON mapping, each `fromJSON` replaced by what
    /// `outputs` says the needed job left. `Err` names what could not be read.
    pub(crate) fn resolve(
        &self,
        outputs: impl Fn(&NeedsOutput) -> Option<String>,
    ) -> Result<serde_json::Map<String, Value>, String> {
        let read = |reference: &NeedsOutput| -> Result<Value, String> {
            let text = outputs(reference).unwrap_or_default();
            if text.trim().is_empty() {
                return Err(format!(
                    "`{reference}` is empty, so `fromJSON` has nothing to read"
                ));
            }
            serde_json::from_str(&text)
                .map_err(|error| format!("`{reference}` is not JSON: {error}"))
        };
        match self {
            Self::Whole(reference) => match read(reference)? {
                Value::Object(entries) => {
                    for (key, value) in &entries {
                        check_entry(key, value).map_err(|(at, reason)| {
                            format!("`{reference}` → {key}{at} {reason}")
                        })?;
                    }
                    Ok(entries)
                }
                _ => Err(format!(
                    "`{reference}` must be a JSON object of matrix dimensions"
                )),
            },
            Self::Entries(entries) => {
                let mut resolved = serde_json::Map::new();
                for (key, entry) in entries {
                    let value = match entry {
                        MatrixEntry::Value(value) => value.clone(),
                        MatrixEntry::FromNeeds(reference) => {
                            let value = read(reference)?;
                            check_entry(key, &value).map_err(|(at, reason)| {
                                format!("`{reference}` for {key}{at} {reason}")
                            })?;
                            value
                        }
                    };
                    resolved.insert(key.clone(), value);
                }
                Ok(resolved)
            }
        }
    }
}

/// `${{ fromJSON(needs.<job>.outputs.<name>) }}` and nothing else around it.
/// GitHub's function names ignore case.
fn from_json_expression(text: &str) -> Option<NeedsOutput> {
    let inner = text.trim().strip_prefix("${{")?.strip_suffix("}}")?.trim();
    let (function, rest) = inner.split_once('(')?;
    if !function.trim().eq_ignore_ascii_case("fromjson") {
        return None;
    }
    let argument = rest.strip_suffix(')')?.trim();
    match rg_process::ci_steps::RuntimeKey::parse(argument)? {
        rg_process::ci_steps::RuntimeKey::NeedsOutput { job, name } => {
            Some(NeedsOutput { job, output: name })
        }
        _ => None,
    }
}

fn from_json_refusal(text: &str) -> String {
    format!(
        "`{text}` is not a matrix this engine can read: a list in the file, or \
         `${{{{ fromJSON(needs.<job>.outputs.<name>) }}}}`"
    )
}

/// A scalar a matrix value may be: one that has a string form for the job's
/// `MATRIX_*` variable.
fn scalar(value: &Value) -> bool {
    matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
}

/// One entry of a matrix mapping: a dimension is a list of scalars, `include`
/// and `exclude` lists of mappings of scalars. `Err` is (path below the key,
/// reason).
fn check_entry(key: &str, value: &Value) -> Result<(), (String, String)> {
    let Value::Array(items) = value else {
        let what = if matches!(key, "include" | "exclude") {
            "a list of mappings"
        } else {
            "a list of strings, numbers, or booleans"
        };
        return Err((String::new(), format!("must be {what}")));
    };
    for (index, item) in items.iter().enumerate() {
        if matches!(key, "include" | "exclude") {
            let Value::Object(fields) = item else {
                return Err((format!("[{index}]"), "must be a mapping".into()));
            };
            for (field, value) in fields {
                if !scalar(value) {
                    return Err((
                        format!("[{index}].{field}"),
                        "must be a string, number, or boolean".into(),
                    ));
                }
            }
        } else if !scalar(item) {
            return Err((
                format!("[{index}]"),
                "must be a string, number, or boolean".into(),
            ));
        }
    }
    Ok(())
}

/// The string form a matrix value takes in a job name and a variable.
pub(crate) fn value_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

/// One variant: its coordinates in the order the variant is labelled with.
pub(crate) type Combination = Vec<(String, String)>;

/// Expand a matrix mapping the way GitHub does.
///
/// - The dimensions (every key but `include` / `exclude`) multiply out.
/// - `exclude` drops each combination one of its entries matches — partly is
///   enough: every key the entry names has to agree.
/// - `include` adds each entry's keys to every original combination none of
///   whose dimension values it would overwrite; an entry that fits none
///   becomes a combination of its own. Values an earlier `include` added may
///   be overwritten; the dimensions never are.
///
/// `Err` is the reason the matrix has no expansion: a dimension with no
/// values, nothing left at all, or more than [`MAX_MATRIX_VARIANTS`].
pub(crate) fn combinations(
    matrix: &serde_json::Map<String, Value>,
) -> Result<Vec<Combination>, String> {
    let list = |key: &str| -> Vec<&Value> {
        matrix
            .get(key)
            .and_then(Value::as_array)
            .map(|items| items.iter().collect())
            .unwrap_or_default()
    };
    let dimensions = matrix
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "include" | "exclude"))
        .map(|(key, values)| (key.clone(), values.as_array().cloned().unwrap_or_default()))
        .collect::<BTreeMap<_, _>>();
    if let Some((key, _)) = dimensions.iter().find(|(_, values)| values.is_empty()) {
        return Err(format!("matrix dimension `{key}` has no values"));
    }

    let mut product: Vec<Vec<(String, Value)>> = if dimensions.is_empty() {
        Vec::new()
    } else {
        let count = dimensions
            .values()
            .try_fold(1usize, |total, values| total.checked_mul(values.len()))
            .filter(|count| *count <= MAX_MATRIX_VARIANTS * 4)
            .ok_or_else(|| {
                format!("matrix is too large to expand; maximum is {MAX_MATRIX_VARIANTS} variants")
            })?;
        let mut product = vec![Vec::with_capacity(dimensions.len())];
        product.reserve(count);
        for (key, values) in &dimensions {
            product = product
                .into_iter()
                .flat_map(|combination| {
                    values.iter().map(move |value| {
                        let mut combination = combination.clone();
                        combination.push((key.clone(), value.clone()));
                        combination
                    })
                })
                .collect();
        }
        product
    };

    let matches = |combination: &[(String, Value)], entry: &serde_json::Map<String, Value>| {
        entry.iter().all(|(key, value)| {
            combination
                .iter()
                .any(|(name, existing)| name == key && existing == value)
        })
    };
    for exclude in list("exclude") {
        if let Some(exclude) = exclude.as_object() {
            product.retain(|combination| !matches(combination, exclude));
        }
    }

    let original = dimensions.keys().cloned().collect::<BTreeSet<_>>();
    let mut extra = Vec::new();
    for include in list("include") {
        let Some(include) = include.as_object() else {
            continue;
        };
        let fields = include.iter().collect::<BTreeMap<_, _>>();
        let mut fitted = false;
        for combination in &mut product {
            let keeps_dimensions = fields.iter().all(|(key, value)| {
                !original.contains(key.as_str())
                    || combination
                        .iter()
                        .any(|(name, existing)| name == *key && *existing == **value)
            });
            if !keeps_dimensions {
                continue;
            }
            fitted = true;
            for (key, value) in &fields {
                match combination.iter_mut().find(|(name, _)| name == *key) {
                    Some((_, existing)) => *existing = (*value).clone(),
                    None => combination.push(((*key).clone(), (*value).clone())),
                }
            }
        }
        if !fitted {
            extra.push(
                fields
                    .into_iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<Vec<_>>(),
            );
        }
    }
    product.extend(extra);

    if product.is_empty() {
        return Err("matrix expands to no variant at all".into());
    }
    if product.len() > MAX_MATRIX_VARIANTS {
        return Err(format!(
            "matrix expands to {} variants; maximum is {MAX_MATRIX_VARIANTS}",
            product.len()
        ));
    }
    Ok(product
        .into_iter()
        .map(|combination| {
            combination
                .into_iter()
                .map(|(key, value)| (key, value_string(&value)))
                .collect()
        })
        .collect())
}

/// How a matrix job's variants run beside each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MatrixStrategy {
    /// A failed variant cancels the variants that have not finished.
    /// GitHub's default is `true`.
    pub(crate) fail_fast: bool,
    /// At most this many variants run at once.
    pub(crate) max_parallel: Option<u32>,
}

impl Default for MatrixStrategy {
    fn default() -> Self {
        Self {
            fail_fast: true,
            max_parallel: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expand(yaml: &str) -> Result<Vec<Combination>, String> {
        let value: Value = rg_core::yaml::from_str(yaml).unwrap();
        combinations(value.as_object().unwrap())
    }

    fn pairs(combination: &[(&str, &str)]) -> Combination {
        combination
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    /// GitHub's own worked example of `include`, from "Expanding or adding
    /// matrix configurations".
    #[test]
    fn include_follows_githubs_worked_example() {
        let expanded = expand(
            "fruit: [apple, pear]\nanimal: [cat, dog]\ninclude:\n  - color: green\n  - color: pink\n    animal: cat\n  - fruit: apple\n    shape: circle\n  - fruit: banana\n  - fruit: banana\n    animal: cat\n",
        )
        .unwrap();
        assert_eq!(
            expanded,
            [
                pairs(&[
                    ("animal", "cat"),
                    ("fruit", "apple"),
                    ("color", "pink"),
                    ("shape", "circle")
                ]),
                pairs(&[("animal", "cat"), ("fruit", "pear"), ("color", "pink")]),
                pairs(&[
                    ("animal", "dog"),
                    ("fruit", "apple"),
                    ("color", "green"),
                    ("shape", "circle")
                ]),
                pairs(&[("animal", "dog"), ("fruit", "pear"), ("color", "green")]),
                pairs(&[("fruit", "banana")]),
                pairs(&[("animal", "cat"), ("fruit", "banana")]),
            ]
        );
    }

    #[test]
    fn exclude_drops_partial_matches_before_include_runs() {
        let expanded = expand(
            "os: [linux, mac]\nnode: [18, 20]\nexclude:\n  - os: mac\n    node: 18\ninclude:\n  - os: mac\n    node: 18\n    experimental: true\n",
        )
        .unwrap();
        assert_eq!(
            expanded,
            [
                pairs(&[("node", "18"), ("os", "linux")]),
                pairs(&[("node", "20"), ("os", "linux")]),
                pairs(&[("node", "20"), ("os", "mac")]),
                pairs(&[("experimental", "true"), ("node", "18"), ("os", "mac")]),
            ],
            "an excluded combination comes back only as an include of its own"
        );
        assert_eq!(
            expand("os: [linux, mac]\nexclude:\n  - os: mac\n").unwrap(),
            [pairs(&[("os", "linux")])]
        );
    }

    #[test]
    fn a_matrix_of_includes_alone_is_its_entries() {
        assert_eq!(
            expand("include:\n  - os: linux\n  - os: mac\n    arch: arm64\n").unwrap(),
            [
                pairs(&[("os", "linux")]),
                pairs(&[("arch", "arm64"), ("os", "mac")])
            ]
        );
    }

    #[test]
    fn a_matrix_with_no_expansion_is_refused_by_reason() {
        assert_eq!(
            expand("os: []\n").unwrap_err(),
            "matrix dimension `os` has no values"
        );
        assert_eq!(
            expand("os: [linux]\nexclude:\n  - os: linux\n").unwrap_err(),
            "matrix expands to no variant at all"
        );
        let values = (0..17)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(
            expand(&format!("a: [{values}]\nb: [{values}]\n")).unwrap_err(),
            "matrix expands to 289 variants; maximum is 256"
        );
        let include = (0..257)
            .map(|n| format!("  - n: {n}\n"))
            .collect::<String>();
        assert_eq!(
            expand(&format!("include:\n{include}")).unwrap_err(),
            "matrix expands to 257 variants; maximum is 256",
            "the cap counts what `include` adds"
        );
        let wide = (0..100)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        assert!(
            expand(&format!(
                "a: [{wide}]\nb: [{wide}]\nc: [{wide}]\nd: [{wide}]\n"
            ))
            .unwrap_err()
            .contains("too large to expand"),
            "a product far past the cap is refused before it is built"
        );
    }

    #[test]
    fn from_json_is_read_only_over_a_needed_jobs_output() {
        let spec = MatrixSpec::parse(&Value::String(
            "${{ fromJson( needs.setup.outputs.matrix ) }}".into(),
        ))
        .unwrap();
        assert_eq!(
            spec,
            MatrixSpec::Whole(NeedsOutput {
                job: "setup".into(),
                output: "matrix".into()
            })
        );
        for refused in [
            "${{ fromJSON(inputs.matrix) }}",
            "${{ needs.setup.outputs.matrix }}",
            "prefix ${{ fromJSON(needs.setup.outputs.matrix) }}",
            "${{ toJSON(needs.setup.outputs.matrix) }}",
        ] {
            assert!(
                MatrixSpec::parse(&Value::String(refused.into())).is_err(),
                "{refused}"
            );
        }
        let entries: Value = rg_core::yaml::from_str(
            "os: ${{ fromJSON(needs.setup.outputs.os) }}\ninclude:\n  - os: linux\n    big: true\n",
        )
        .unwrap();
        let spec = MatrixSpec::parse(&entries).unwrap();
        assert_eq!(
            spec.needs_outputs(),
            [&NeedsOutput {
                job: "setup".into(),
                output: "os".into()
            }]
        );
        let resolved = spec
            .resolve(|_| Some(r#"["linux", "mac"]"#.into()))
            .unwrap();
        assert_eq!(
            combinations(&resolved).unwrap(),
            [
                pairs(&[("os", "linux"), ("big", "true")]),
                pairs(&[("os", "mac")]),
            ]
        );
    }

    #[test]
    fn what_a_needed_job_left_is_checked_like_the_file() {
        let whole = MatrixSpec::Whole(NeedsOutput {
            job: "setup".into(),
            output: "matrix".into(),
        });
        for (output, named) in [
            ("", "`needs.setup.outputs.matrix` is empty"),
            ("{nope", "`needs.setup.outputs.matrix` is not JSON"),
            ("[1, 2]", "must be a JSON object of matrix dimensions"),
            (r#"{"os": "linux"}"#, "os must be a list of strings"),
            (
                r#"{"os": [{"a": 1}]}"#,
                "os[0] must be a string, number, or boolean",
            ),
            (r#"{"include": [1]}"#, "include[0] must be a mapping"),
        ] {
            let error = whole
                .resolve(|_| Some(output.to_string()))
                .expect_err(output);
            assert!(error.contains(named), "{output}: {error}");
        }
    }
}
