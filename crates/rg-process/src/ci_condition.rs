//! Safe, deliberately small evaluator for CI `if` expressions.
//!
//! Two scopes read the same grammar. A **job** condition is evaluated once:
//! against the event when the pipeline graph is written, or — for a job that
//! lists `needs:` — once its stage is reached, when it may also read what the
//! needed jobs left. A job that needs none has only `success()`, always true:
//! nothing ran before it. For one that lists `needs:` the status functions
//! answer for the jobs it needs and the jobs those need ([`JobStatus`]), so
//! `if: failure()` runs it after one of them failed. A **step** condition is
//! evaluated by the runner between two steps, so the status functions answer
//! for the steps that ran before it — `success()`, `failure()`, `always()`,
//! `cancelled()`. In either scope a condition that names none of them means
//! `success() && <condition>`, as on GitHub. The runner is either this process
//! or an external one, which is why the evaluator lives below both.
//!
//! A string literal is single-quoted and a quote inside it is doubled
//! (`'it''s'`), as on GitHub; a backslash is an ordinary character.

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
pub const GITHUB_CONTEXT_KEYS: [&str; 9] = [
    "github.ref",
    "github.ref_name",
    "github.event_name",
    "github.sha",
    "github.repository",
    "github.repository_owner",
    // The `- cron:` entry of a `schedule` run; the empty string otherwise.
    "github.event.schedule",
    // The source and target branch of a `pull_request` run, by short name;
    // the empty string for every other event, as on GitHub.
    "github.head_ref",
    "github.base_ref",
];

/// What the status functions of a step condition answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StepStatus {
    /// An earlier step failed and did not continue on error.
    pub failed: bool,
    /// The job was canceled while it ran.
    pub cancelled: bool,
}

/// What the status functions of the condition of a job that lists `needs:`
/// answer, once those jobs have finished.
///
/// They speak for every job it needs and every job those need in turn, as on
/// GitHub: `notify: needs: test, if: failure()` runs when `build`, which `test`
/// needs, failed — `test` itself was skipped, never run. `cancelled()` is
/// `false`: a job is decided only while its pipeline is live, and a canceled
/// one runs nothing more.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobStatus {
    /// One of those jobs failed.
    pub any_failed: bool,
    /// Every one of them succeeded — none failed, was canceled or skipped.
    pub all_succeeded: bool,
}

/// Whose status the status functions of a condition answer for.
#[derive(Clone, Copy, Debug)]
enum Status {
    Step(StepStatus),
    Job(JobStatus),
}

/// The status functions, lower-cased, as a condition names them.
const STATUS_FUNCTIONS: [&str; 4] = ["success", "failure", "always", "cancelled"];

/// The names a condition may read, whatever its scope.
fn known_context_name(name: &str) -> bool {
    GITHUB_CONTEXT_KEYS.contains(&name)
        || name.strip_prefix("env.").is_some_and(valid_context_key)
        || name.strip_prefix("matrix.").is_some_and(valid_matrix_key)
}

/// A matrix key may carry `-` as well — `node-version` is the common one —
/// since it is a label, and only its derived `MATRIX_*` has to be a variable
/// name.
fn valid_matrix_key(key: &str) -> bool {
    key.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_')
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

pub fn validate_condition(input: &str) -> Result<()> {
    evaluate_with(
        input,
        |name| known_context_name(name).then(String::new),
        None,
    )
    .map(|_| ())
}

/// [`validate_condition`] for a job that lists `needs:`: it may read what the
/// needed jobs left — `needs.<job>.outputs.<name>` and `needs.<job>.result` —
/// because such a job is decided only once they have finished, and the status
/// functions answer for them ([`JobStatus`]). Not `steps.*`: a job's `if:` is
/// read before any of its steps run. Whether each one names a job this one
/// needs is the workflow reader's question.
///
/// `cancelled()` is refused unless it is negated: a canceled pipeline runs
/// nothing more, so a job asking to run after a cancellation would never run.
/// `!cancelled()` — GitHub's "after success or failure" — is honoured.
pub fn validate_job_condition(input: &str) -> Result<()> {
    evaluate_with(
        input,
        |name| (known_context_name(name) || needs_key(name)).then(String::new),
        Some(Status::Job(JobStatus::default())),
    )?;
    let tokens = tokenize(unwrap_expression(input)?)?;
    for (index, token) in tokens.iter().enumerate() {
        let calls_cancelled = matches!(token, Token::Ident(name) if name.eq_ignore_ascii_case("cancelled"))
            && tokens.get(index + 1) == Some(&Token::LParen);
        let negated = index > 0 && tokens[index - 1] == Token::Not;
        if calls_cancelled && !negated {
            bail!(
                "cancelled() asks for the job to run after its pipeline was canceled, and a \
                 canceled pipeline runs nothing more; `!cancelled()` is honoured"
            );
        }
    }
    Ok(())
}

/// The status functions a condition calls, lower-cased, outside its quoted
/// strings. Empty for one that does not parse; the validator is what refuses
/// that.
pub fn status_functions(input: &str) -> std::collections::BTreeSet<String> {
    let Ok(tokens) = unwrap_expression(input).and_then(tokenize) else {
        return Default::default();
    };
    tokens
        .windows(2)
        .filter_map(|pair| match pair {
            [Token::Ident(name), Token::LParen] => {
                let lower = name.to_ascii_lowercase();
                STATUS_FUNCTIONS.contains(&lower.as_str()).then_some(lower)
            }
            _ => None,
        })
        .collect()
}

/// Whether a job condition may be true after a job it needs failed — it calls
/// a status function other than `success()`. Such a job is still decided when
/// an earlier stage failed, rather than skipped with the rest.
pub fn runs_after_failure(input: &str) -> bool {
    status_functions(input)
        .iter()
        .any(|function| function != "success")
}

/// Whether a condition reads `needs.*`, outside its quoted strings — whether it
/// has an answer only once the jobs it needs have finished. `false` for one
/// that does not parse; the validator is what refuses that.
pub fn reads_needs(input: &str) -> bool {
    unwrap_expression(input)
        .and_then(tokenize)
        .is_ok_and(|tokens| {
            tokens
                .iter()
                .any(|token| matches!(token, Token::Ident(name) if needs_key(name)))
        })
}

fn needs_key(name: &str) -> bool {
    matches!(
        crate::ci_steps::RuntimeKey::parse(name),
        Some(
            crate::ci_steps::RuntimeKey::NeedsOutput { .. }
                | crate::ci_steps::RuntimeKey::NeedsResult(_)
        )
    )
}

/// [`validate_condition`] for a step: the status functions are accepted too,
/// and so are the contexts only a runner has between steps —
/// `steps.<id>.outputs.<name>`, `steps.<id>.outcome` / `.conclusion`,
/// `needs.<job>.outputs.<name>` and `needs.<job>.result`. Whether each one
/// names a step or a job that exists is the workflow reader's question.
pub fn validate_step_condition(input: &str) -> Result<()> {
    evaluate_with(
        input,
        |name| {
            (known_context_name(name) || crate::ci_steps::RuntimeKey::parse(name).is_some())
                .then(String::new)
        },
        Some(Status::Step(StepStatus::default())),
    )
    .map(|_| ())
}

fn lookup(context: &HashMap<String, String>, name: &str) -> Option<String> {
    context.get(name).cloned().or_else(|| {
        // An output nobody wrote, or the result of a step that never ran, is
        // the empty string on GitHub too.
        (name.starts_with("env.")
            || name.starts_with("matrix.")
            || crate::ci_steps::RuntimeKey::parse(name).is_some())
        .then(String::new)
    })
}

/// The value an expression evaluates to, as text — `${{ a && b || '' }}`
/// is the empty string or `b`, not `false`. Only the names `context` holds
/// may be read; any other is an error, so a caller can tell an expression it
/// cannot answer from one whose answer is empty.
pub fn evaluate_text(input: &str, context: &HashMap<String, String>) -> Result<String> {
    let expression = unwrap_expression(input)?;
    let tokens = tokenize(expression)?;
    let resolver = |name: &str| context.get(name).cloned();
    let mut parser = Parser {
        tokens,
        position: 0,
        resolver: &resolver,
        depth: 0,
        status: None,
        named_status: false,
    };
    let result = parser.parse_or()?;
    if parser.peek() != &Token::End {
        bail!("unexpected token after expression");
    }
    Ok(result.text())
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
    let outcome = evaluate_with(
        input,
        |name| lookup(context, name),
        Some(Status::Step(status)),
    )?;
    Ok(if outcome.named_status {
        outcome.value
    } else {
        outcome.value && !status.failed && !status.cancelled
    })
}

/// Evaluate the condition of a job that lists `needs:`, once they finished.
///
/// One that names no status function is read as `success() && <condition>`,
/// as a step condition is.
pub fn evaluate_job_condition(
    input: &str,
    context: &HashMap<String, String>,
    status: JobStatus,
) -> Result<bool> {
    let outcome = evaluate_with(
        input,
        |name| lookup(context, name),
        Some(Status::Job(status)),
    )?;
    Ok(if outcome.named_status {
        outcome.value
    } else {
        outcome.value && status.all_succeeded
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
    status: Option<Status>,
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
                        // A doubled quote is one quote of the text, as on
                        // GitHub — and as every substitution here writes a
                        // value into a literal (`vars.*`, an action's
                        // `inputs.*`). A backslash escaped it once, which
                        // read `'it''s'` as two strings and `'C:\'` as one
                        // that never closes.
                        if chars.get(i + 1) == Some(&quote) {
                            value.push(quote);
                            i += 2;
                            continue;
                        }
                        i += 1;
                        closed = true;
                        break;
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
                // `-` belongs to a name after its first character:
                // `steps.my-step.outputs.my-value` is one property path, and
                // this language has no subtraction for it to be mistaken for.
                while i < chars.len()
                    && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '_' | '.' | '-'))
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
    /// `Some` for a step condition and for that of a job that lists `needs:`,
    /// whose status functions have an answer.
    status: Option<Status>,
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
            // The operand, not a boolean, as on GitHub: `a || 'default'`
            // is a value. Truthiness — all a condition reads — is the same.
            left = if left.truthy() { left } else { right };
        }
        Ok(left)
    }
    fn parse_and(&mut self) -> Result<Value> {
        let mut left = self.parse_equality()?;
        while self.peek() == &Token::And {
            self.take();
            let right = self.parse_equality()?;
            left = if left.truthy() { right } else { left };
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
                // The condition of a job that needs no other job: nothing ran
                // before it, so only `success()` has an answer, and it is yes.
                if lower == "success" {
                    return Ok(Value::Bool(true));
                }
                bail!(
                    "{name}() has no meaning in the condition of a job that lists no `needs:`: \
                     in a job condition it answers for the jobs it needs"
                );
            };
            self.named_status = true;
            return Ok(Value::Bool(match (lower.as_str(), status) {
                ("success", Status::Step(step)) => !step.failed && !step.cancelled,
                ("failure", Status::Step(step)) => step.failed,
                ("cancelled", Status::Step(step)) => step.cancelled,
                ("success", Status::Job(job)) => job.all_succeeded,
                ("failure", Status::Job(job)) => job.any_failed,
                ("cancelled", Status::Job(_)) => false,
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

    /// `&&` and `||` give back an operand, as on GitHub — the default of
    /// `actions/setup-node`'s `token:` is `''` here, not `false` — while a
    /// condition, which reads only truthiness, answers as before.
    #[test]
    fn and_and_or_give_back_an_operand() {
        let context = HashMap::from([
            ("github.server_url".to_string(), String::new()),
            ("github.token".to_string(), String::new()),
            ("env.NAME".to_string(), "x".to_string()),
        ]);
        let text = |input: &str| evaluate_text(input, &context).unwrap();
        assert_eq!(
            text("${{ github.server_url == 'https://github.com' && github.token || '' }}"),
            ""
        );
        assert_eq!(text("env.NAME || 'fallback'"), "x");
        assert_eq!(text("'' || 'fallback'"), "fallback");
        assert_eq!(text("env.NAME && 'both'"), "both");
        assert!(evaluate_text("github.unknown || ''", &context).is_err());
        assert!(evaluate_condition("env.NAME && 'both'", &context).unwrap());
        assert!(!evaluate_condition("'' || ''", &context).unwrap());
    }

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

    /// A job that lists `needs:` may read what they left, and nothing a step
    /// would: its `if:` is read before its first step runs.
    #[test]
    fn a_job_condition_reads_needs_but_not_steps() {
        for accepted in [
            "needs.changes.outputs.backend == 'true'",
            "${{ needs.build.result == 'success' && github.ref == 'refs/heads/main' }}",
            "contains(needs.my-job.outputs.list, 'x')",
        ] {
            validate_job_condition(accepted).unwrap_or_else(|error| panic!("{accepted}: {error}"));
            assert!(reads_needs(accepted), "{accepted}");
            assert!(validate_condition(accepted).is_err(), "{accepted}");
        }
        for refused in [
            "steps.a.outputs.b == 'x'",
            "needs.build.outputs == 'x'",
            "cancelled() || needs.build.result == 'failure'",
            "secrets.TOKEN == 'x'",
        ] {
            assert!(validate_job_condition(refused).is_err(), "{refused}");
        }
        assert!(!reads_needs("github.ref == 'needs.build.result'"));
        assert!(!reads_needs("env.needs == 'x'"));

        let context = HashMap::from([
            (
                "needs.changes.outputs.backend".to_string(),
                "true".to_string(),
            ),
            ("needs.changes.result".to_string(), "success".to_string()),
        ]);
        assert!(evaluate_condition("needs.changes.outputs.backend == 'true'", &context).unwrap());
        assert!(!evaluate_condition("needs.changes.outputs.frontend == 'true'", &context).unwrap());
    }

    /// card_29fa305fba93: the status functions of a job that lists `needs:`
    /// answer for those jobs, and a condition naming none of them is
    /// `success() && <condition>`.
    #[test]
    fn job_status_functions_answer_for_the_jobs_it_needs() {
        let context = HashMap::from([("github.ref".into(), "refs/heads/main".into())]);
        let succeeded = JobStatus {
            any_failed: false,
            all_succeeded: true,
        };
        let failed = JobStatus {
            any_failed: true,
            all_succeeded: false,
        };
        let skipped = JobStatus {
            any_failed: false,
            all_succeeded: false,
        };
        for (condition, after_success, after_failure, after_skip) in [
            ("success()", true, false, false),
            ("failure()", false, true, false),
            ("always()", true, true, true),
            ("!cancelled()", true, true, true),
            ("github.ref == 'refs/heads/main'", true, false, false),
            (
                "${{ failure() && github.ref == 'refs/heads/main' }}",
                false,
                true,
                false,
            ),
        ] {
            validate_job_condition(condition)
                .unwrap_or_else(|error| panic!("{condition}: {error}"));
            for (status, expected) in [
                (succeeded, after_success),
                (failed, after_failure),
                (skipped, after_skip),
            ] {
                assert_eq!(
                    evaluate_job_condition(condition, &context, status).unwrap(),
                    expected,
                    "`{condition}` with {status:?}"
                );
            }
        }
        for refused in [
            "cancelled()",
            "${{ cancelled() || failure() }}",
            "always(1)",
        ] {
            assert!(validate_job_condition(refused).is_err(), "{refused}");
        }
        // A job that needs none still has only `success()`.
        assert!(validate_condition("failure()").is_err());
        assert!(validate_condition("success()").is_ok());
    }

    #[test]
    fn status_functions_are_read_outside_strings() {
        assert_eq!(
            status_functions("${{ failure() && github.ref == 'always()' }}"),
            ["failure".to_string()].into()
        );
        assert!(status_functions("github.ref == 'failure()'").is_empty());
        assert!(runs_after_failure("Always()"));
        assert!(runs_after_failure("!cancelled()"));
        assert!(!runs_after_failure(
            "success() && needs.a.result == 'success'"
        ));
        assert!(!runs_after_failure("needs.a.result == 'failure'"));
    }

    /// A quote inside a literal is doubled, as on GitHub and as the `vars.*`
    /// and action-input substitutions write it; a backslash is text.
    #[test]
    fn a_doubled_quote_is_one_quote_of_the_literal() {
        let context = HashMap::from([("github.ref_name".into(), "it's".into())]);
        assert!(evaluate_condition("github.ref_name == 'it''s'", &context).unwrap());
        assert_eq!(
            evaluate_text("'C:\\' || 'x'", &context).unwrap(),
            "C:\\",
            "a backslash does not escape the closing quote"
        );
        assert_eq!(evaluate_text("''''", &context).unwrap(), "'");
        assert!(validate_condition("'it's'").is_err());
    }
}
