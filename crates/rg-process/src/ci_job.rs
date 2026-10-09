//! The two rules a CI job's *inputs to a child process* obey on both sides of
//! the runner API.
//!
//! The embedded runner (`rg-ci`, inside the server) and the external agent
//! (`rg-runner`, a standalone binary that deliberately links none of the
//! server's crates) each turn a job row into a `docker run` and each collect
//! what the job printed. Two copies of either rule is how they drifted apart
//! once already: the server bounded the log it would *accept* from an agent
//! while both executors still collected the whole thing in memory first. This
//! crate is the one both link, so the rules live here and `rg_core::ci`
//! re-exports them for the server side.

/// The most of a job's output either runner keeps, and the most one log upload
/// may carry.
///
/// The runner posts a job's whole output in a single request, so this is the
/// ceiling on an entire build's log rather than on a chunk of one — and a
/// verbose build (`cargo build -v`, `npm ci`, `docker build`) clears Axum's
/// hidden 2 MiB `DefaultBodyLimit` without trying. Left unstated, that default
/// turned a long build's diagnosis into a job with no log at all.
///
/// The number is bounded by what the log then costs downstream rather than by
/// what a build can print: the body is masked in memory, broadcast whole to
/// every websocket subscriber of the job, and stored by rewriting the job row's
/// whole `log` column. Both executors stop *retaining* output at this ceiling
/// while still draining the pipes, so a `yes | head -c 20G` cannot grow the
/// server by what it prints; what was cut is said at the end of the log.
pub const JOB_LOG_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Longest image reference accepted; far above anything a registry stores,
/// well below anything that costs to hold.
const IMAGE_REFERENCE_MAX_LEN: usize = 4096;

/// Longest tag the distribution spec allows after the leading character.
const TAG_REST_MAX_LEN: usize = 127;

/// Check that `image` is an OCI distribution reference and nothing else.
///
/// `image:` is passed to `docker run` as the positional image argument. Both
/// executors now put `--` in front of it, so the Docker CLI can no longer read
/// `image: "--privileged"` or `image: "-v/:/host"` as a flag — but a value that
/// *looks* like a flag is still a job that was never going to run, and a value
/// such as `--env-file=/etc/plombir-git/plombir-git.toml` used to leak the
/// first lines of the server's config through Docker's own parse error into the
/// job log. The file is refused at trigger time with the rule it broke, and the
/// executors ask again right before spawning, because the job row they read was
/// written by whatever server version validated it.
///
/// The grammar is the distribution reference grammar
/// (`[registry[:port]/]path[:tag][@sha256:<64 hex>]`):
///
/// - path components are `[a-z0-9]+` joined by single separators `.`, `_`,
///   `__` or one or more `-`, and separated from each other by `/`;
/// - the registry is recognised the way Docker does it — the first component
///   is a host when it contains `.` or `:`, is `localhost`, or carries an
///   uppercase letter — and may carry a `:port`;
/// - the tag is `[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}`;
/// - the only digest accepted is `sha256:` followed by 64 hex digits.
///
/// ASCII only, no whitespace, no leading `-`, at most 4096 bytes in all. The
/// `Err` names the rule for the author of the file.
pub fn validate_image_reference(image: &str) -> Result<(), String> {
    if image.is_empty() {
        return Err("image is empty".into());
    }
    if image.len() > IMAGE_REFERENCE_MAX_LEN {
        return Err(format!(
            "image is longer than {IMAGE_REFERENCE_MAX_LEN} bytes"
        ));
    }
    if let Some(offender) = image
        .chars()
        .find(|c| !c.is_ascii() || c.is_ascii_whitespace() || c.is_ascii_control())
    {
        return Err(format!(
            "image contains {}; only printable ASCII without whitespace is allowed",
            describe_char(offender)
        ));
    }
    if image.starts_with('-') {
        return Err("image starts with '-', which is a docker flag, not an image".into());
    }

    // `name[:tag][@digest]` — the digest is split off first because it is the
    // only part that may itself contain `:`.
    let (name_and_tag, digest) = match image.split_once('@') {
        Some((head, digest)) => (head, Some(digest)),
        None => (image, None),
    };
    if let Some(digest) = digest {
        validate_digest(digest)?;
    }

    // A `:` after the last `/` is the tag separator; one before it belongs to
    // the registry's port.
    let last_slash = name_and_tag.rfind('/');
    let (name, tag) = match name_and_tag.rfind(':') {
        Some(colon) if last_slash.is_none_or(|slash| colon > slash) => {
            (&name_and_tag[..colon], Some(&name_and_tag[colon + 1..]))
        }
        _ => (name_and_tag, None),
    };
    if let Some(tag) = tag {
        validate_tag(tag)?;
    }
    validate_name(name)
}

fn describe_char(c: char) -> String {
    if c.is_ascii_whitespace() {
        "whitespace".to_string()
    } else if c.is_ascii_control() {
        "a control character".to_string()
    } else {
        format!("the non-ASCII character '{c}'")
    }
}

fn validate_digest(digest: &str) -> Result<(), String> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(format!(
            "image digest '{digest}' is not 'sha256:<64 hex digits>'"
        ));
    };
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "image digest '{digest}' is not 'sha256:<64 hex digits>'"
        ));
    }
    Ok(())
}

fn validate_tag(tag: &str) -> Result<(), String> {
    let mut chars = tag.chars();
    let valid_first = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    let rest = chars.as_str();
    let valid_rest = rest.len() <= TAG_REST_MAX_LEN
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !valid_first || !valid_rest {
        return Err(format!(
            "image tag '{tag}' is not [A-Za-z0-9_][A-Za-z0-9_.-]{{0,{TAG_REST_MAX_LEN}}}"
        ));
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("image has no name".into());
    }
    let mut components = name.split('/').peekable();
    let first = components.peek().copied().unwrap_or_default();
    // Docker's rule, verbatim: `foo/bar` is the `foo` namespace on the default
    // registry, `foo.example/bar`, `foo:5000/bar`, `localhost/bar` and
    // `Foo/bar` all name a registry. A single component is never one.
    let names_a_registry = name.contains('/')
        && (first.contains('.')
            || first.contains(':')
            || first == "localhost"
            || first.chars().any(|c| c.is_ascii_uppercase()));
    if names_a_registry {
        validate_registry(first)?;
        components.next();
    }
    let mut any = false;
    for component in components {
        any = true;
        validate_path_component(component)?;
    }
    if !any {
        return Err(format!("image '{name}' names a registry but no repository"));
    }
    Ok(())
}

fn validate_registry(registry: &str) -> Result<(), String> {
    let (host, port) = match registry.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (registry, None),
    };
    if let Some(port) = port {
        if port.is_empty() || port.len() > 5 || !port.chars().all(|c| c.is_ascii_digit()) {
            return Err(format!("image registry port '{port}' is not a number"));
        }
    }
    let valid_host = !host.is_empty()
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        });
    if !valid_host {
        return Err(format!(
            "image registry '{registry}' is not a host name with an optional :port"
        ));
    }
    Ok(())
}

/// `[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*`
fn validate_path_component(component: &str) -> Result<(), String> {
    let invalid = || {
        format!(
            "image path component '{component}' is not lowercase [a-z0-9] runs joined by '.', \
             '_', '__' or '-'"
        )
    };
    if component.is_empty() {
        return Err(invalid());
    }
    let bytes = component.as_bytes();
    let is_run_char = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    let mut index = 0;
    loop {
        // A run of `[a-z0-9]+`.
        let run_start = index;
        while index < bytes.len() && is_run_char(bytes[index]) {
            index += 1;
        }
        if index == run_start {
            return Err(invalid());
        }
        if index == bytes.len() {
            return Ok(());
        }
        // A separator: `.`, `_`, `__` or `-+`, always followed by another run.
        match bytes[index] {
            b'.' => index += 1,
            b'_' => {
                index += 1;
                if bytes.get(index) == Some(&b'_') {
                    index += 1;
                }
            }
            b'-' => {
                while bytes.get(index) == Some(&b'-') {
                    index += 1;
                }
            }
            _ => return Err(invalid()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn well_formed_references_are_accepted() {
        for image in [
            "nginx",
            "nginx:1.27",
            "library/nginx",
            "ghcr.io/org/img",
            &format!("ghcr.io/org/img@{DIGEST}"),
            &format!("ghcr.io/org/img:v1@{DIGEST}"),
            "localhost:5000/x:y",
            "localhost/x",
            "registry.example.com:5000/a/b/c:v1.2",
            "registry.example.com/a/b/c",
            "Registry.Example.COM/a",
            "rust:1.75-slim-bookworm",
            "my__image/sub-component.v2",
            "alpine:3.20_rc1",
            "fixture:success",
            "x:_tag.with-every.class",
            "127.0.0.1:5000/image",
        ] {
            assert_eq!(validate_image_reference(image), Ok(()), "{image}");
        }
    }

    #[test]
    fn docker_flags_are_refused_whatever_the_separator() {
        for image in [
            "--privileged",
            "-v/:/host",
            "-v",
            "--env-file=x",
            "--env-file=/etc/plombir-git/plombir-git.toml",
            "-",
        ] {
            let error = validate_image_reference(image).unwrap_err();
            assert!(error.contains("docker flag"), "{image}: {error}");
        }
    }

    #[test]
    fn whitespace_unicode_control_and_empty_are_refused() {
        assert!(validate_image_reference("").unwrap_err().contains("empty"));
        for image in [" nginx", "nginx ", "ngi nx", "nginx\tlatest", "nginx\n"] {
            let error = validate_image_reference(image).unwrap_err();
            assert!(error.contains("whitespace"), "{image:?}: {error}");
        }
        let error = validate_image_reference("nginx\u{1}").unwrap_err();
        assert!(error.contains("control"), "{error}");
        for image in ["ngïnx", "образ:latest", "nginx\u{200b}"] {
            let error = validate_image_reference(image).unwrap_err();
            assert!(error.contains("non-ASCII"), "{image:?}: {error}");
        }
    }

    #[test]
    fn an_uppercase_path_is_not_a_repository() {
        // The spec lowercases repositories; `Nginx` is refused rather than
        // folded, because folding would run a different image than written.
        let error = validate_image_reference("Nginx").unwrap_err();
        assert!(error.contains("path component 'Nginx'"), "{error}");
        let error = validate_image_reference("org/Nginx:latest").unwrap_err();
        assert!(error.contains("path component 'Nginx'"), "{error}");
    }

    #[test]
    fn malformed_components_tags_digests_and_registries_are_refused() {
        for image in [
            "nginx/",
            "/nginx",
            "org//nginx",
            "org/-nginx",
            "nginx-",
            "ng..inx",
            "ng___inx",
            "nginx:",
            "nginx:-bad",
            "nginx:tag/with/slash",
            &format!("nginx:{}", "t".repeat(129)),
            "nginx@sha256:abc",
            "nginx@md5:00000000000000000000000000000000",
            &format!("nginx@{}", &DIGEST[..DIGEST.len() - 1]),
            "localhost:port/x",
            "localhost:5000/",
            "-.example.com/x",
            "a-.example.com/x",
            "registry.example.com::5000/x",
        ] {
            assert!(validate_image_reference(image).is_err(), "{image}");
        }
        assert!(validate_image_reference(&"a".repeat(IMAGE_REFERENCE_MAX_LEN)).is_ok());
        assert!(validate_image_reference(&"a".repeat(IMAGE_REFERENCE_MAX_LEN + 1)).is_err());
        assert!(validate_image_reference(&format!("nginx:{}", "t".repeat(128))).is_ok());
    }
}
