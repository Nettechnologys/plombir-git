//! The one shape a job's `image:` may have before it reaches `docker run`.
//!
//! The image is the only argument of `docker run` the repository author
//! chooses, and it used to be placed straight after the runner's flags: the
//! Docker CLI read `image: --privileged` as a flag, and `-v/:/host` as a mount.
//! [`crate::docker_job::run_args`] now ends the options with `--`, and this
//! check refuses anything that is not an image reference at all, so a bad value
//! is the author's error with the rule named instead of a `docker` parse error.
//!
//! The grammar is the distribution reference grammar Docker itself parses:
//! `[host[:port]/]path[:tag][@algorithm:hex]`.

/// Docker's own limit on the name part (everything before `:tag` / `@digest`).
const MAX_NAME_BYTES: usize = 255;

/// Why `image` is not an image reference.
pub fn validate_image_reference(image: &str) -> Result<(), String> {
    let refuse = |rule: &str| {
        Err(format!(
            "image '{}' is not an image reference: {rule}",
            image.escape_debug()
        ))
    };

    let (rest, digest) = match image.split_once('@') {
        Some((rest, digest)) => (rest, Some(digest)),
        None => (image, None),
    };
    if let Some(digest) = digest {
        if !is_digest(digest) {
            return refuse(
                "a digest is sha256:, sha384: or sha512: followed by the lowercase hex of that \
                 length",
            );
        }
    }
    // A `:` after the last `/` starts the tag; one before it is a registry port.
    let last_slash = rest.rfind('/').map_or(0, |at| at + 1);
    let (name, tag) = match rest[last_slash..].rfind(':') {
        Some(at) => (&rest[..last_slash + at], Some(&rest[last_slash + at + 1..])),
        None => (rest, None),
    };
    if let Some(tag) = tag {
        if !is_tag(tag) {
            return refuse("a tag is up to 128 of [A-Za-z0-9_.-] and does not start with . or -");
        }
    }
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return refuse("the name must be 1 to 255 bytes long");
    }

    let mut components = name.split('/');
    let first = components.next().unwrap_or_default();
    let path_start = if name.contains('/') && looks_like_registry(first) {
        if !is_registry(first) {
            return refuse("the registry is a host name, an IPv4 address or a [IPv6] literal, with an optional :port");
        }
        components.collect::<Vec<_>>()
    } else {
        name.split('/').collect()
    };
    if path_start.is_empty()
        || !path_start
            .iter()
            .all(|component| is_path_component(component))
    {
        return refuse(
            "each path component is lowercase letters and digits, joined by '.', '_', '__' or \
             dashes",
        );
    }
    Ok(())
}

/// Docker reads the first component as a registry when it could not be a
/// repository path: it has a `.` or a `:`, is `localhost`, or is bracketed.
fn looks_like_registry(component: &str) -> bool {
    component.contains(['.', ':'])
        || component == "localhost"
        || component.starts_with('[')
        || component.bytes().any(|byte| byte.is_ascii_uppercase())
}

fn is_registry(registry: &str) -> bool {
    let (host, port) = if let Some(bracketed) = registry.strip_prefix('[') {
        let Some((literal, after)) = bracketed.split_once(']') else {
            return false;
        };
        if literal.parse::<std::net::Ipv6Addr>().is_err() {
            return false;
        }
        match after {
            "" => return true,
            _ => match after.strip_prefix(':') {
                Some(port) => ("", Some(port)),
                None => return false,
            },
        }
    } else {
        match registry.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (registry, None),
        }
    };
    let port_ok = port.is_none_or(|port| {
        !port.is_empty() && port.len() <= 5 && port.bytes().all(|byte| byte.is_ascii_digit())
    });
    let host_ok = (host.is_empty() && registry.starts_with('['))
        || (!host.is_empty()
            && host.split('.').all(|label| {
                let bytes = label.as_bytes();
                !bytes.is_empty()
                    && bytes[0].is_ascii_alphanumeric()
                    && bytes[bytes.len() - 1].is_ascii_alphanumeric()
                    && bytes
                        .iter()
                        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
            }));
    port_ok && host_ok
}

/// `[a-z0-9]+` runs joined by `.`, `_`, `__` or any number of `-`.
fn is_path_component(component: &str) -> bool {
    let bytes = component.as_bytes();
    let alnum = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if bytes.is_empty() || !alnum(&bytes[0]) || !alnum(&bytes[bytes.len() - 1]) {
        return false;
    }
    let mut at = 0;
    while at < bytes.len() {
        if alnum(&bytes[at]) {
            at += 1;
            continue;
        }
        let separator_start = at;
        while at < bytes.len() && !alnum(&bytes[at]) {
            at += 1;
        }
        let separator = &component[separator_start..at];
        let valid =
            matches!(separator, "." | "_" | "__") || separator.bytes().all(|byte| byte == b'-');
        if !valid {
            return false;
        }
    }
    true
}

fn is_tag(tag: &str) -> bool {
    let bytes = tag.as_bytes();
    let word = |byte: &u8| byte.is_ascii_alphanumeric() || *byte == b'_';
    !bytes.is_empty()
        && bytes.len() <= 128
        && word(&bytes[0])
        && bytes
            .iter()
            .all(|byte| word(byte) || *byte == b'.' || *byte == b'-')
}

fn is_digest(digest: &str) -> bool {
    let Some((algorithm, hex)) = digest.split_once(':') else {
        return false;
    };
    let length = match algorithm {
        "sha256" => 64,
        "sha384" => 96,
        "sha512" => 128,
        _ => return false,
    };
    hex.len() == length
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::validate_image_reference;

    #[test]
    fn references_docker_accepts_are_accepted() {
        let sha256 = format!("sha256:{}", "a".repeat(64));
        let sha512 = format!("sha512:{}", "0f".repeat(64));
        for image in [
            "alpine".to_string(),
            "alpine:3.20".to_string(),
            "rust:1.75".to_string(),
            "library/postgres:17".to_string(),
            "registry.example/private:latest".to_string(),
            "registry.example:5000/team/app_name/sub-project:v1.2.3".to_string(),
            "localhost/app".to_string(),
            "localhost:5000/app:dev".to_string(),
            "127.0.0.1:5000/app".to_string(),
            "[::1]/app".to_string(),
            "[2001:db8::1]:5000/team/app:tag".to_string(),
            "a__b/c---d".to_string(),
            format!("alpine@{sha256}"),
            format!("alpine:3.20@{sha256}"),
            format!("ghcr.io/owner/app@{sha512}"),
        ] {
            assert_eq!(validate_image_reference(&image), Ok(()), "{image}");
        }
    }

    #[test]
    fn values_that_are_not_an_image_are_refused() {
        for image in [
            "",
            "--privileged",
            "-v/:/host",
            "--env-file=/etc/plombir-git/plombir-git.toml",
            "-it",
            "alpine --privileged",
            "alpine\n",
            "Alpine",
            "alpine:",
            ":latest",
            "alpine:-tag",
            "alpine:.tag",
            "/alpine",
            "alpine/",
            "team//app",
            "team/-app",
            "team/app.",
            "team/a..b",
            "registry.example:/app",
            "registry.example:port/app",
            "-registry.example/app",
            "[::1/app",
            "[not-an-ip]/app",
            "[::1]x/app",
            "alpine@sha256:abc",
            "alpine@md5:0123456789abcdef0123456789abcdef",
            "alpine@sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "alpine@",
            "élan",
        ] {
            assert!(
                validate_image_reference(image).is_err(),
                "`{}` must be refused",
                image.escape_debug()
            );
        }
        assert!(validate_image_reference(&"a".repeat(256)).is_err());
        assert!(validate_image_reference(&format!("alpine:{}", "t".repeat(129))).is_err());
    }

    #[test]
    fn the_refusal_names_the_value_and_the_rule() {
        let error = validate_image_reference("--privileged").unwrap_err();
        assert!(error.contains("'--privileged'"), "{error}");
        assert!(error.contains("path component"), "{error}");
    }
}
