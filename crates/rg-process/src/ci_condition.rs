//! Safe, deliberately small evaluator for CI `if` expressions.
//!
//! Two scopes read the same grammar. A **job** condition is evaluated once,
//! against the event, when the pipeline graph is written: only `success()` has
//! a meaning there, and it is always true. A **step** condition is evaluated by
//! the runner between two steps, so the status functions answer for the steps
//! that ran before it — `success()`, `failure()`, `always()`, `cancelled()` —
//! and a condition that names none of them means `success() && <condition>`,
//! as on GitHub. The runner is either this process or an external one, which is
//! why the evaluator lives below both.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
enum Value {
    Bool(bool),
    String(String),
}
impl Value {
    fn truthy(&self) -> bool {
        match self {
            Self::Bool(value) => *value,
            Self::String(value) => !value.is_empty(),
        }
    }
    fn text(&self) -> String {
        match self {
            Self::Bool(value) => value.to_string(),
            Self::String(value) => value.clone(),
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
enum Token {
    Ident(String),
    String(String),
    Bool(bool),
    Eq,
    Ne,
    And,
    Or,
    Not,
    LParen,
    RParen,
    Comma,
    End,
}

/// The `github.*` names an `if:` condition may read.
///
/// The list is public and enumerable because it is one of *three* places that
/// have to agree: this validator decides whether a condition is accepted at all,
/// and two separate context builders decide what it evaluates to —
/// `gitea_actions::actions_condition_context` for step conditions at conversion
/// time and `job_condition_context` for job conditions at graph-write time. A
/// name present here and missing from a builder evaluates to the empty string,
/// which a comparison reports as a plain `false` — a silently wrong answer, not
/// an error. `every_condition_key_the_validator_accepts_is_answered_by_both_contexts`
/// holds the three together.
pub const GITHUB_CONTEXT_KEYS: [&str; 7] = [
    "github.ref",
    "github.ref_name",
    "github.event_name",
    "github.sha",
    "github.repository",
    "github.repository_owner",
    // The `- cron:` entry of a `schedule` run; the empty string otherwise.
    "github.event.schedule",
];

/// What the status functions of a step condition answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StepStatus {
    /// An earlier step failed and did not continue on error.
    pub failed: bool,
    /// The job was canceled while it ran.
    pub cancelled: bool,
}

/// The names a condition may read, whatever its scope.
fn known_context_name(name: &str) -> bool {
    GITHUB_CONTEXT_KEYS.contains(&name)
        || name.strip_prefix("env.").is_some_and(valid_context_key)
        || name.strip_prefix("matrix.").is_some_and(valid_context_key)
}

pub fn validate_condition(input: &str) -> Result<()> {
    evaluate_with(
        input,
        |name| known_context_name(name).then(String::new),
        None,
    )
    .map(|_| ())
}

/// [`validate_condition`] for a step: the status functions are accepted too.
pub fn validate_step_condition(input: &str) -> Result<()> {
    evaluate_with(
        input,
        |name| known_context_name(name).then(String::new),
        Some(StepStatus::default()),
    )
    .map(|_| ())
}

fn lookup(context: &HashMap<String, String>, name: &str) -> Option<String> {
    context
        .get(name)
        .cloned()
        .or_else(|| (name.starts_with("env.") || name.starts_with("matrix.")).then(String::new))
}

pub fn evaluate_condition(input: &str, context: &HashMap<String, String>) -> Result<bool> {
    evaluate_with(input, |name| lookup(context, name), None).map(|outcome| outcome.value)
}

/// Evaluate a step condition between two steps.
///
/// One that names no status function is read as `success() && <condition>`:
/// written as `if: github.ref == 'refs/heads/main'`, it does not run after a
/// failed step, which is what GitHub does and what an author who never wrote
/// `always()` expects.
pub fn evaluate_step_condition(
    input: &str,
    context: &HashMap<String, String>,
    status: StepStatus,
) -> Result<bool> {
    let outcome = evaluate_with(input, |name| lookup(context, name), Some(status))?;
    Ok(if outcome.named_status {
        outcome.value
    } else {
        outcome.value && !status.failed && !status.cancelled
    })
}

fn valid_context_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

struct Outcome {
    value: bool,
    /// Whether the condition called a status function itself.
    named_status: bool,
}

fn evaluate_with(
    input: &str,
    resolver: impl Fn(&str) -> Option<String>,
    status: Option<StepStatus>,
) -> Result<Outcome> {
    let expression = unwrap_expression(input)?;
    let tokens = tokenize(expression)?;
    let mut parser = Parser {
        tokens,
        position: 0,
        resolver: &resolver,
        depth: 0,
        status,
        named_status: false,
    };
    let result = parser.parse_or()?;
    if parser.peek() != &Token::End {
        bail!("unexpected token after condition expression");
    }
    Ok(Outcome {
        value: result.truthy(),
        named_status: parser.named_status,
    })
}

fn unwrap_expression(input: &str) -> Result<&str> {
    let value = input.trim();
    if let Some(inner) = value.strip_prefix("${{") {
        return inner
            .strip_suffix("}}")
            .map(str::trim)
            .context("condition is missing closing '}}'");
    }
    Ok(value)
}

fn tokenize(input: &str) -> Result<Vec<Token>> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            ch if ch.is_whitespace() => i += 1,
            '(' => {
                out.push(Token::LParen);
                i += 1;
            }
            ')' => {
                out.push(Token::RParen);
                i += 1;
            }
            ',' => {
                out.push(Token::Comma);
                i += 1;
            }
            '!' if chars.get(i + 1) == Some(&'=') => {
                out.push(Token::Ne);
                i += 2;
            }
            '!' => {
                out.push(Token::Not);
                i += 1;
            }
            '=' if chars.get(i + 1) == Some(&'=') => {
                out.push(Token::Eq);
                i += 2;
            }
            '&' if chars.get(i + 1) == Some(&'&') => {
                out.push(Token::And);
                i += 2;
            }
            '|' if chars.get(i + 1) == Some(&'|') => {
                out.push(Token::Or);
                i += 2;
            }
            quote @ ('\'' | '"') => {
                i += 1;
                let mut value = String::new();
                let mut closed = false;
                while i < chars.len() {
                    if chars[i] == quote {
                        i += 1;
                        closed = true;
                        break;
                    }
                    if chars[i] == '\\' {
                        i += 1;
                        if i >= chars.len() {
                            bail!("unterminated escape in condition string");
                        }
                    }
                    value.push(chars[i]);
                    i += 1;
                }
                if !closed {
                    bail!("unterminated condition string");
                }
                out.push(Token::String(value));
            }
            ch if ch.is_ascii_alphanumeric() || ch == '_' => {
                let start = i;
                i += 1;
                while i < chars.len()
                    && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '_' | '.'))
                {
                    i += 1;
                }
                let ident: String = chars[start..i].iter().collect();
                match ident.as_str() {
                    "true" => out.push(Token::Bool(true)),
                    "false" => out.push(Token::Bool(false)),
                    _ => out.push(Token::Ident(ident)),
                }
            }
            ch => bail!("unsupported character '{ch}' in condition"),
        }
    }
    out.push(Token::End);
    Ok(out)
}

/// How deeply a condition may nest `(`, function arguments and `!`.
///
/// The parser is a recursive descent, and a stack overflow in Rust is not an
/// error the caller can catch: it aborts the whole server, HTTP and SSH alike.
/// The condition comes from a workflow file anyone with push access writes, and
/// it is parsed synchronously on the post-push path, so `"!".repeat(1 << 20)`
/// in an `if:` was a one-push crash. Real conditions nest a handful of levels.
const MAX_NESTING: usize = 64;

struct Parser<'a, F> {
    tokens: Vec<Token>,
    position: usize,
    resolver: &'a F,
    depth: usize,
    /// `Some` for a step condition, whose status functions have an answer.
    status: Option<StepStatus>,
    named_status: bool,
}
impl<'a, F: Fn(&str) -> Option<String>> Parser<'a, F> {
    /// Run `step` one nesting level deeper, refusing past [`MAX_NESTING`].
    fn nested(&mut self, step: impl FnOnce(&mut Self) -> Result<Value>) -> Result<Value> {
        if self.depth >= MAX_NESTING {
            bail!("condition nests deeper than {MAX_NESTING} levels");
        }
        self.depth += 1;
        let value = step(self);
        self.depth -= 1;
        value
    }
    fn peek(&self) -> &Token {
        self.tokens.get(self.position).unwrap_or(&Token::End)
    }
    fn take(&mut self) -> Token {
        let token = self.peek().clone();
        self.position += 1;
        token
    }
    fn parse_or(&mut self) -> Result<Value> {
        let mut left = self.parse_and()?;
        while self.peek() == &Token::Or {
            self.take();
            let right = self.parse_and()?;
            left = Value::Bool(left.truthy() || right.truthy());
        }
        Ok(left)
    }
    fn parse_and(&mut self) -> Result<Value> {
        let mut left = self.parse_equality()?;
        while self.peek() == &Token::And {
            self.take();
            let right = self.parse_equality()?;
            left = Value::Bool(left.truthy() && right.truthy());
        }
        Ok(left)
    }
    fn parse_equality(&mut self) -> Result<Value> {
        let left = self.parse_unary()?;
        match self.peek() {
            Token::Eq => {
                self.take();
                let right = self.parse_unary()?;
                Ok(Value::Bool(
                    left == right || left.text().eq_ignore_ascii_case(&right.text()),
                ))
            }
            Token::Ne => {
                self.take();
                let right = self.parse_unary()?;
                Ok(Value::Bool(
                    !(left == right || left.text().eq_ignore_ascii_case(&right.text())),
                ))
            }
            _ => Ok(left),
        }
    }
    fn parse_unary(&mut self) -> Result<Value> {
        if self.peek() == &Token::Not {
            self.take();
            let operand = self.nested(Self::parse_unary)?;
            return Ok(Value::Bool(!operand.truthy()));
        }
        self.parse_primary()
    }
    fn parse_primary(&mut self) -> Result<Value> {
        match self.take() {
            Token::Bool(value) => Ok(Value::Bool(value)),
            Token::String(value) => Ok(Value::String(value)),
            Token::LParen => {
                let value = self.nested(Self::parse_or)?;
                if self.take() != Token::RParen {
                    bail!("condition is missing ')'");
                }
                Ok(value)
            }
            Token::Ident(name) if self.peek() == &Token::LParen => self.parse_function(&name),
            Token::Ident(name) => (self.resolver)(&name)
                .map(Value::String)
                .with_context(|| format!("unsupported or unavailable condition context '{name}'")),
            token => bail!("unexpected token in condition: {token:?}"),
        }
    }
    fn parse_function(&mut self, name: &str) -> Result<Value> {
        self.take();
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "success" | "failure" | "always" | "cancelled"
        ) {
            if self.take() != Token::RParen {
                bail!("{name}() does not accept arguments");
            }
            let Some(status) = self.status else {
                // A job condition is read once, before anything has run: only
                // `success()` has an answer there, and it is always yes.
                if lower == "success" {
                    return Ok(Value::Bool(true));
                }
                bail!(
                    "{name}() has no meaning in a job condition, which is evaluated before any \
                     job has run; it is a step condition function"
                );
            };
            self.named_status = true;
            return Ok(Value::Bool(match lower.as_str() {
                "success" => !status.failed && !status.cancelled,
                "failure" => status.failed,
                "cancelled" => status.cancelled,
                _ => true,
            }));
        }
        if !matches!(lower.as_str(), "startswith" | "endswith" | "contains") {
            bail!("unsupported condition function '{name}'");
        }
        let left = self.nested(Self::parse_or)?;
        if self.take() != Token::Comma {
            bail!("{name}() requires two arguments");
        }
        let right = self.nested(Self::parse_or)?;
        if self.take() != Token::RParen {
            bail!("{name}() requires two arguments");
        }
        let (left, right) = (left.text(), right.text());
        Ok(Value::Bool(match lower.as_str() {
            "startswith" => left.starts_with(&right),
            "endswith" => left.ends_with(&right),
            _ => left.contains(&right),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse `input` on a thread with a small stack, the way a hostile `if:`
    /// would meet the parser: without the nesting limit these inputs overflow
    /// it, which aborts the whole test binary rather than failing one test.
    fn validate_on_a_small_stack(input: String) -> Result<()> {
        std::thread::Builder::new()
            .stack_size(1 << 20)
            .spawn(move || validate_condition(&input))
            .expect("spawn the parser thread")
            .join()
            .expect("the parser thread panicked")
    }

    #[test]
    fn a_condition_nested_past_the_limit_is_refused_not_a_stack_overflow() {
        for hostile in [
            format!("{}true", "!".repeat(1_000_000)),
            format!("{}true{}", "(".repeat(1_000_000), ")".repeat(1_000_000)),
            format!(
                "{}'a', 'b'{}",
                "contains(".repeat(200_000),
                ")".repeat(200_000)
            ),
        ] {
            let error = validate_on_a_small_stack(hostile)
                .expect_err("a condition nested a million levels deep must be refused");
            assert!(
                error.to_string().contains("nests deeper than"),
                "the refusal must say why: {error}"
            );
        }
    }

    #[test]
    fn a_condition_nested_up_to_the_limit_still_parses() {
        let depth = MAX_NESTING - 1;
        let parens = format!("{}true{}", "(".repeat(depth), ")".repeat(depth));
        validate_on_a_small_stack(parens)
            .expect("nesting below the limit is an ordinary condition");
        let negations = format!("{}true", "!".repeat(depth));
        validate_on_a_small_stack(negations).expect("negation below the limit is ordinary");
    }
    #[test]
    fn evaluates_boolean_context_and_string_functions() {
        let context = HashMap::from([
            ("github.ref".into(), "refs/heads/main".into()),
            ("env.DEPLOY".into(), "yes".into()),
        ]);
        assert!(evaluate_condition(
            "github.ref == 'refs/heads/main' && env.DEPLOY == 'yes'",
            &context
        )
        .unwrap());
        assert!(evaluate_condition(
            "${{ startsWith(github.ref, 'refs/heads/') && !false }}",
            &context
        )
        .unwrap());
        assert!(!evaluate_condition("contains(github.ref, 'tags') || false", &context).unwrap());
    }
    #[test]
    fn string_comparisons_ignore_ascii_case() {
        let context = HashMap::from([("github.ref_name".into(), "Release".into())]);
        assert!(evaluate_condition("github.ref_name == 'release'", &context).unwrap());
        assert!(!evaluate_condition("github.ref_name != 'RELEASE'", &context).unwrap());
    }
    #[test]
    fn step_status_functions_answer_for_the_steps_before() {
        let context = HashMap::from([("github.ref".into(), "refs/heads/main".into())]);
        let healthy = StepStatus::default();
        let failed = StepStatus {
            failed: true,
            cancelled: false,
        };
        let cancelled = StepStatus {
            failed: false,
            cancelled: true,
        };
        for (condition, after_success, after_failure, after_cancel) in [
            ("success()", true, false, false),
            ("failure()", false, true, false),
            ("always()", true, true, true),
            ("cancelled()", false, false, true),
            // No status function: an implicit `success() &&`.
            ("github.ref == 'refs/heads/main'", true, false, false),
            (
                "failure() && github.ref == 'refs/heads/main'",
                false,
                true,
                false,
            ),
            ("${{ always() }}", true, true, true),
        ] {
            for (status, expected) in [
                (healthy, after_success),
                (failed, after_failure),
                (cancelled, after_cancel),
            ] {
                assert_eq!(
                    evaluate_step_condition(condition, &context, status).unwrap(),
                    expected,
                    "`{condition}` with {status:?}"
                );
            }
            validate_step_condition(condition).unwrap();
        }
        assert!(validate_step_condition("always(1)").is_err());
    }

    #[test]
    fn rejects_unknown_context_functions_and_syntax() {
        assert!(validate_condition("secrets.TOKEN == 'x'").is_err());
        assert!(validate_condition("hashFiles('**')").is_err());
        assert!(validate_condition("always()").is_err());
        assert!(validate_condition("failure()").is_err());
        assert!(validate_condition("github.ref ==").is_err());
    }
}
